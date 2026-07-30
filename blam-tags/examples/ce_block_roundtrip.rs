//! Gate: can every *property block* in the shipped corpus be written back
//! byte-exactly from its decoded values alone?
//!
//! The header gate (`ce_header_roundtrip`) proved the block's *frame* can be
//! regenerated. This proves the contents can too, which is the claim that
//! decides whether `PropValue` is a lossless model of what the file holds or
//! merely a convenient view of it. A value the reader quietly rounded, reordered
//! or dropped shows up here as a byte difference and nowhere else — every one of
//! them decodes back to something perfectly plausible.
//!
//! It compares against the span the reader consumed, not against the whole
//! export: what follows a block is the class's natively serialized tail, which
//! is Phase 4's problem.
//!
//! Run: `ce_block_roundtrip [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::block::emit_block;
use blam_tags::iostore::object::unversioned::read_export_struct_len;
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

/// Classes whose `Serialize` never calls `Super`, so they have no block.
const NO_PROPERTY_BLOCK: &[&str] = &["RigVM", "RigHierarchy"];

/// The first offset at which two byte strings differ.
fn first_difference(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()))
}

fn main() {
    let usmap_path = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap").into()
    });
    let mut usmap = match std::fs::read(&usmap_path) {
        Ok(b) => Usmap::parse(&b).expect("parse usmap"),
        Err(_) => Usmap::meteorite().expect("bundled usmap"),
    };
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);

    let mut by_hash: HashMap<u64, String> = HashMap::new();
    match ScriptObjects::load(format!("{PAKS}/global.utoc")) {
        Ok(so) => {
            for e in so.entries() {
                if let Some(p) = so.resolve(e.global_index.raw_index()) {
                    by_hash.insert(e.global_index.raw_index(), p.to_string());
                }
            }
        }
        Err(e) => {
            eprintln!("no script-object table ({e:#}); cannot resolve classes");
            std::process::exit(2);
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

    let (mut total, mut same, mut unreadable, mut unwritable) = (0usize, 0usize, 0usize, 0usize);
    let mut differ_by_class: BTreeMap<String, usize> = BTreeMap::new();
    let mut refuse_by_reason: BTreeMap<String, usize> = BTreeMap::new();
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
            for ex in &h.export_map {
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                if NO_PROPERTY_BLOCK.contains(&short) {
                    continue;
                }
                if usmap.flattened_properties(short).is_none() {
                    continue;
                }
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(b.len());
                if off >= b.len() || off > end {
                    continue;
                }
                let body = &b[off..end];
                // Only the block's own span is in scope; the tail after it is
                // Phase 4.
                let Ok((block, used)) = read_export_struct_len(body, &names, &usmap, short) else {
                    unreadable += 1;
                    continue;
                };
                total += 1;
                match emit_block(short, &block, &usmap) {
                    Ok(out) if out == body[..used] => same += 1,
                    Ok(out) => {
                        *differ_by_class.entry(short.to_string()).or_default() += 1;
                        if samples.len() < 8 {
                            let at = first_difference(&out, &body[..used]);
                            let lo = at.saturating_sub(8);
                            samples.push(format!(
                                "{} :: {short}\n    first difference at byte {at} of {used} \
                                 (wrote {} bytes)\n    orig  {:02x?}\n    ours  {:02x?}",
                                h.package_name(),
                                out.len(),
                                &body[lo..(at + 8).min(used)],
                                &out[lo..(at + 8).min(out.len())],
                            ));
                        }
                    }
                    Err(err) => {
                        unwritable += 1;
                        // Group by the message's head so the reasons collapse
                        // into a handful of causes rather than one line each.
                        // The root cause, not the outermost context: the
                        // "writing property X" frame names the site, and the
                        // cause underneath it is what actually has to be fixed.
                        let reason = err.chain().last().map(|c| c.to_string()).unwrap_or_default();
                        let reason = reason.chars().take(96).collect::<String>();
                        *refuse_by_reason.entry(reason).or_default() += 1;
                    }
                }
            }
        }
    }

    println!("blocks examined      {total}");
    println!("written exactly      {same} ({:.4}%)", 100.0 * same as f64 / total.max(1) as f64);
    println!("differ               {}", total - same - unwritable);
    println!("refused to write     {unwritable}");
    println!("unreadable (skipped) {unreadable}");

    if !refuse_by_reason.is_empty() {
        println!("\nrefusals by reason:");
        let mut v: Vec<_> = refuse_by_reason.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (r, n) in v.iter().take(15) {
            println!("  {n:>7}  {r}");
        }
    }
    if !differ_by_class.is_empty() {
        println!("\ndiffering by class:");
        let mut v: Vec<_> = differ_by_class.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (c, n) in v.iter().take(15) {
            println!("  {n:>7}  {c}");
        }
        for s in &samples {
            println!("\n{s}");
        }
    }
    if same != total {
        std::process::exit(1);
    }
}
