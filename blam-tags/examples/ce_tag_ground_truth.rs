//! EXHAUSTIVE ground truth for every cooked Campaign Evolved tag package.
//!
//! Emits one JSON object per tag to stdout (JSONL) carrying every fact a
//! from-scratch `.uasset` builder must reproduce, plus everything needed to
//! reconcile anomalies:
//!   package/object names, ids and hashes (with the derived candidates)
//!   summary flags, export/bulk shape
//!   the FULL import map, each slot classified
//!   imported package list + ordering checks
//!   dependency bundle contents
//!   every decoded export property, with Object refs resolved to package paths
//!   the tag blob's own tag_reference set
//!
//! Run: cargo run --release --features iostore --example ce_tag_ground_truth > truth.jsonl

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufWriter, Cursor, Write};

use blam_tags::api::TagStruct;
use blam_tags::fields::{TagFieldData, TagFieldType};
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageId, FPackageObjectIndex};
use blam_tags::iostore::unversioned::{read_export_struct, PropValue};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::writer::cityhash64;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::TagFile;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap");
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn ch(s: &str) -> u64 {
    cityhash64(&s.to_ascii_lowercase().encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>())
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

fn jstr(s: &str) -> String {
    format!("\"{}\"", esc(s))
}

fn jarr<T: AsRef<str>>(v: &[T]) -> String {
    format!("[{}]", v.iter().map(|x| jstr(x.as_ref())).collect::<Vec<_>>().join(","))
}

/// Render a PropValue as JSON, resolving Object indices via `resolve`.
fn jprop(v: &PropValue, resolve: &dyn Fn(i32) -> Option<(String, u64)>) -> String {
    match v {
        PropValue::Bool(b) => format!("{{\"k\":\"bool\",\"v\":{b}}}"),
        PropValue::Int(i) => format!("{{\"k\":\"int\",\"v\":{i}}}"),
        PropValue::Float(f) => format!("{{\"k\":\"float\",\"v\":{f}}}"),
        PropValue::Name(n) => format!("{{\"k\":\"name\",\"v\":{}}}", jstr(n)),
        PropValue::Str(s) => format!("{{\"k\":\"str\",\"v\":{}}}", jstr(s)),
        PropValue::Object(i) => match resolve(*i) {
            Some((pkg, hash)) => format!(
                "{{\"k\":\"obj\",\"i\":{i},\"pkg\":{},\"hash\":\"{hash:016x}\"}}",
                jstr(&pkg)
            ),
            None => format!("{{\"k\":\"obj\",\"i\":{i},\"pkg\":null}}"),
        },
        PropValue::SoftObject(p) => format!(
            "{{\"k\":\"soft\",\"pkg\":{},\"asset\":{},\"sub\":{}}}",
            jstr(&p.package), jstr(&p.asset), jstr(&p.sub_path)
        ),
        PropValue::Array(a) => format!(
            "{{\"k\":\"arr\",\"v\":[{}]}}",
            a.iter().map(|x| jprop(x, resolve)).collect::<Vec<_>>().join(",")
        ),
        PropValue::Set(a) => format!(
            "{{\"k\":\"set\",\"v\":[{}]}}",
            a.iter().map(|x| jprop(x, resolve)).collect::<Vec<_>>().join(",")
        ),
        PropValue::Map(m) => format!(
            "{{\"k\":\"map\",\"v\":[{}]}}",
            m.iter()
                .map(|(k, v)| format!("[{},{}]", jprop(k, resolve), jprop(v, resolve)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        // A container carrying a non-empty removal prefix; the ground truth is
        // about the container's contents, so report the count and unwrap.
        PropValue::WithRemovals { removals, inner } => format!(
            "{{\"k\":\"removals\",\"n\":{},\"v\":{}}}",
            match removals {
                Some(r) => r.len() as i64,
                None => -1,
            },
            jprop(inner, resolve)
        ),
        PropValue::Struct(s) => format!(
            "{{\"k\":\"struct\",\"v\":{{{}}}}}",
            s.iter()
                .map(|(k, v)| format!("{}:{}", jstr(k), jprop(v, resolve)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        PropValue::Native(n) => format!("{{\"k\":\"native\",\"v\":{}}}", jstr(&format!("{n:?}"))),
        PropValue::HandWritten(h) => format!("{{\"k\":\"handwritten\",\"v\":{}}}", jstr(&format!("{h:?}"))),
        PropValue::Delegate { object, function } => {
            format!("{{\"k\":\"delegate\",\"o\":{object},\"f\":{}}}", jstr(function))
        }
        PropValue::MulticastDelegate(list) => {
            format!("{{\"k\":\"multicast\",\"n\":{}}}", list.len())
        }
        PropValue::FieldPath { path, owner } => {
            format!("{{\"k\":\"fieldpath\",\"n\":{},\"o\":{owner}}}", path.len())
        }
        PropValue::Unset => "{\"k\":\"unset\"}".to_string(),
        PropValue::Raw(b) => format!("{{\"k\":\"raw\",\"n\":{}}}", b.len()),
    }
}

fn collect_refs(s: &TagStruct, out: &mut BTreeSet<(u32, String)>) {
    for f in s.fields_all() {
        match f.field_type() {
            TagFieldType::TagReference => {
                if let Some(TagFieldData::TagReference(r)) = f.value() {
                    if let Some((g, path)) = r.group_tag_and_name {
                        let p = path.replace('\u{0}', "").trim().replace('\\', "/").to_ascii_lowercase();
                        if !p.is_empty() {
                            out.insert((g, p));
                        }
                    }
                }
            }
            TagFieldType::Struct => {
                if let Some(sub) = f.as_struct() {
                    collect_refs(&sub, out);
                }
            }
            TagFieldType::Block => {
                if let Some(b) = f.as_block() {
                    for el in b.iter() {
                        collect_refs(&el, out);
                    }
                }
            }
            TagFieldType::Array => {
                if let Some(a) = f.as_array() {
                    for el in a.iter() {
                        collect_refs(&el, out);
                    }
                }
            }
            _ => {}
        }
    }
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

fn main() {
    let usmap = Usmap::parse(&std::fs::read(USMAP).expect("usmap")).expect("usmap");
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        let pak = u.file_stem().unwrap().to_string_lossy().to_string();
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") {
                continue;
            }
            let stem = lower.rsplit('/').next().unwrap().trim_end_matches(".uasset");
            let Some((_, group)) = stem.rsplit_once('-') else { continue };
            let group = group.to_string();
            let Ok(ua) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&ua), None, CV, HV, None)
            else {
                writeln!(out, "{{\"path\":{},\"error\":\"header parse\"}}", jstr(&e.path)).ok();
                continue;
            };

            let pkg = h.package_name();
            let obj = h
                .export_map
                .first()
                .map(|x| h.name_map.get(x.object_name).to_string())
                .unwrap_or_default();
            let class_path = format!("/Script/BlamSynchronization.{}", class_for_group(&group));
            let cdo_path = format!("/Script/BlamSynchronization.Default__{}", class_for_group(&group));

            let dep: BTreeSet<i32> = h
                .dependency_bundle_entries
                .iter()
                .map(|d| d.local_import_or_export_index.index)
                .collect();

            let resolve = |i: i32| -> Option<(String, u64)> {
                if i >= 0 {
                    return None;
                }
                let r = h.import_map.get((-i - 1) as usize)?.package_import()?;
                let p = h.imported_package_names.get(r.imported_package_index as usize)?;
                let hash = *h
                    .imported_public_export_hashes
                    .get(r.imported_public_export_hash_index as usize)?;
                Some((p.clone(), hash))
            };

            // ---- import map, fully classified ----
            let mut imports = Vec::new();
            for (i, im) in h.import_map.iter().enumerate() {
                let pi = -(i as i32) - 1;
                let in_dep = dep.contains(&pi);
                if im.is_null() {
                    imports.push(format!("{{\"i\":{i},\"kind\":\"null\",\"dep\":{in_dep}}}"));
                } else if let Some(r) = im.package_import() {
                    let p = h
                        .imported_package_names
                        .get(r.imported_package_index as usize)
                        .cloned()
                        .unwrap_or_default();
                    let hash = h
                        .imported_public_export_hashes
                        .get(r.imported_public_export_hash_index as usize)
                        .copied()
                        .unwrap_or(0);
                    let leaf = p.rsplit('/').next().unwrap_or("");
                    let rule = if ch(&format!("{leaf}_C")) == hash {
                        "leaf_C"
                    } else if ch(leaf) == hash {
                        "leaf"
                    } else {
                        "other"
                    };
                    imports.push(format!(
                        "{{\"i\":{i},\"kind\":\"pkg\",\"pkgi\":{},\"hi\":{},\"pkg\":{},\"hash\":\"{hash:016x}\",\"rule\":\"{rule}\",\"dep\":{in_dep}}}",
                        r.imported_package_index, r.imported_public_export_hash_index, jstr(&p)
                    ));
                } else {
                    let v = im.raw_index();
                    let which = if *im == FPackageObjectIndex::create_script_import(&class_path) {
                        "class"
                    } else if *im == FPackageObjectIndex::create_script_import(&cdo_path) {
                        "cdo"
                    } else if v == 0x24D5BCDF3D9D342 {
                        "module"
                    } else {
                        "script_other"
                    };
                    imports.push(format!(
                        "{{\"i\":{i},\"kind\":\"script\",\"which\":\"{which}\",\"hash\":\"{v:016X}\",\"dep\":{in_dep}}}"
                    ));
                }
            }

            // ---- properties ----
            let mut props_json = String::from("{}");
            let mut prop_class = String::new();
            if let Some(ex) = h.export_map.first() {
                let names = h.name_map.copy_raw_names();
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(ua.len());
                if off < ua.len() {
                    let body = &ua[off..end];
                    let cls = class_for_group(&group);
                    let (p, used) = match read_export_struct(body, &names, &usmap, &cls) {
                        Ok(p) => (Some(p), cls.clone()),
                        Err(_) => match read_export_struct(body, &names, &usmap, "BlamTagDataAssetBase") {
                            Ok(p) => (Some(p), "BlamTagDataAssetBase".to_string()),
                            Err(err) => {
                                writeln!(out, "{{\"path\":{},\"error\":{}}}", jstr(&e.path), jstr(&err.to_string())).ok();
                                (None, String::new())
                            }
                        },
                    };
                    prop_class = used;
                    if let Some(p) = p {
                        props_json = format!(
                            "{{{}}}",
                            p.iter()
                                .map(|(k, v)| format!("{}:{}", jstr(k), jprop(v, &resolve)))
                                .collect::<Vec<_>>()
                                .join(",")
                        );
                    }
                }
            }

            // ---- blob refs ----
            let ubulk = e.path.replace(".uasset", ".ubulk");
            let mut blob_refs: Vec<String> = Vec::new();
            let mut blob_ok = false;
            let mut blob_len = 0i64;
            if let Ok(blob) = a.read(&ubulk) {
                blob_len = blob.len() as i64;
                if let Ok(tag) = TagFile::read_from_bytes(&blob) {
                    blob_ok = true;
                    let mut set = BTreeSet::new();
                    collect_refs(&tag.root(), &mut set);
                    blob_refs = set
                        .into_iter()
                        .map(|(g, p)| {
                            let be = String::from_utf8_lossy(&g.to_be_bytes()).trim().to_string();
                            format!("{be}|{p}")
                        })
                        .collect();
                }
            }

            // ---- ordering checks ----
            let ids_sorted = h.imported_packages.windows(2).all(|w| w[0].0 <= w[1].0);
            let ids_match = h
                .imported_package_names
                .iter()
                .zip(h.imported_packages.iter())
                .all(|(n, id)| FPackageId::from_name(n) == *id);
            let mut expect: Vec<u64> = Vec::new();
            let mut seen_pair: BTreeSet<(u32, u64)> = BTreeSet::new();
            for im in &h.import_map {
                if let Some(r) = im.package_import() {
                    if let Some(hv) = h
                        .imported_public_export_hashes
                        .get(r.imported_public_export_hash_index as usize)
                    {
                        if seen_pair.insert((r.imported_package_index, *hv)) {
                            expect.push(*hv);
                        }
                    }
                }
            }
            let hashes_rule = expect == h.imported_public_export_hashes;

            let ex0 = h.export_map.first();
            let bulk0 = h.bulk_data.first();
            writeln!(
                out,
                "{{\"pak\":{},\"path\":{},\"pkg\":{},\"obj\":{},\"group\":{},\
\"class_hash\":\"{:016X}\",\"class_derived\":\"{:016X}\",\"cdo_hash\":\"{:016X}\",\"cdo_derived\":\"{:016X}\",\
\"pub_hash\":\"{:016x}\",\"pub_derived\":\"{:016x}\",\"pkgid\":\"{:016x}\",\"pkgid_derived\":\"{:016x}\",\
\"pkg_flags\":\"{:x}\",\"obj_flags\":\"{:x}\",\"unversioned\":{},\"header_size\":{},\"file_size\":{},\
\"n_exports\":{},\"n_bulk\":{},\"bulk_flags\":{},\"bulk_size\":{},\"serial_size\":{},\
\"outer_null\":{},\"super_null\":{},\"n_names\":{},\"names\":{},\
\"imported_pkgs\":{},\"ids_sorted\":{},\"ids_match\":{},\"hashes_rule\":{},\"n_hashes\":{},\
\"dep_header\":[{},{},{},{}],\"n_dep\":{},\"imports\":[{}],\
\"prop_class\":{},\"props\":{},\"blob_ok\":{},\"blob_len\":{},\"blob_refs\":{},\
\"trailer\":\"{}\"}}",
                jstr(&pak),
                jstr(&e.path),
                jstr(&pkg),
                jstr(&obj),
                jstr(&group),
                ex0.map(|x| x.class_index.raw_index()).unwrap_or(0),
                FPackageObjectIndex::create_script_import(&class_path).raw_index(),
                ex0.map(|x| x.template_index.raw_index()).unwrap_or(0),
                FPackageObjectIndex::create_script_import(&cdo_path).raw_index(),
                ex0.map(|x| x.public_export_hash).unwrap_or(0),
                ch(&obj),
                FPackageId::from_name(&pkg).0,
                ch(&pkg),
                h.summary.package_flags,
                ex0.map(|x| x.object_flags).unwrap_or(0),
                h.is_unversioned,
                h.summary.header_size,
                ua.len(),
                h.export_map.len(),
                h.bulk_data.len(),
                bulk0.map(|b| b.flags as i64).unwrap_or(-1),
                bulk0.map(|b| b.serial_size).unwrap_or(-1),
                ex0.map(|x| x.cooked_serial_size as i64).unwrap_or(-1),
                ex0.map(|x| x.outer_index.is_null()).unwrap_or(false),
                ex0.map(|x| x.super_index.is_null()).unwrap_or(false),
                h.name_map.copy_raw_names().len(),
                jarr(&h.name_map.copy_raw_names()),
                jarr(&h.imported_package_names),
                ids_sorted,
                ids_match,
                hashes_rule,
                h.imported_public_export_hashes.len(),
                h.dependency_bundle_headers.first().map(|d| d.create_before_create_dependencies).unwrap_or(0),
                h.dependency_bundle_headers.first().map(|d| d.serialize_before_create_dependencies).unwrap_or(0),
                h.dependency_bundle_headers.first().map(|d| d.create_before_serialize_dependencies).unwrap_or(0),
                h.dependency_bundle_headers.first().map(|d| d.serialize_before_serialize_dependencies).unwrap_or(0),
                h.dependency_bundle_entries.len(),
                imports.join(","),
                jstr(&prop_class),
                props_json,
                blob_ok,
                blob_len,
                jarr(&blob_refs),
                {
                    let s = h.summary.header_size as usize
                        + ex0.map(|x| (x.cooked_serial_offset + x.cooked_serial_size) as usize).unwrap_or(0);
                    if s + 4 <= ua.len() {
                        ua[s..s + 4].iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join("")
                    } else {
                        String::new()
                    }
                }
            )
            .ok();
        }
    }
    let _ = out.flush();
    let _: BTreeMap<(), ()> = BTreeMap::new();
}
