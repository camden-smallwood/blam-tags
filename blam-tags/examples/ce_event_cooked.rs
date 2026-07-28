//! Decode a Campaign Evolved `UAkAudioEvent` export properly — via the `.usmap`
//! reflection schema rather than by pattern-matching its name map.
//!
//! `UAkAudioEvent::EventCookedData` is an `FWwiseLocalizedEventCookedData`:
//!
//!   TMap<FWwiseLanguageCookedData, FWwiseEventCookedData> EventLanguageMap
//!   FName DebugName
//!   int32 EventId
//!
//! and each `FWwiseEventCookedData` carries `EventId`, `SoundBanks[]`,
//! `Media[]`, `ExternalSources[]`, … So the language→media binding is explicit
//! in the data; guessing from the name map (which loses it for localized VO,
//! where N languages share one source `.wav`) is unnecessary.
//!
//! Run:
//!   cargo run --release -p blam-tags --features iostore --example ce_event_cooked -- <event-substr>

use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::unversioned::{read_export_struct, PropValue};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn i(v: Option<&PropValue>) -> i64 {
    match v {
        Some(PropValue::Int(n)) => *n,
        _ => 0,
    }
}
fn s(v: Option<&PropValue>) -> String {
    v.and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn b(v: Option<&PropValue>) -> bool {
    matches!(v, Some(PropValue::Bool(true)))
}

/// One playable media entry, fully qualified by the language it belongs to.
#[derive(Debug)]
pub struct Media {
    pub language: String,
    pub media_id: i64,
    pub path: String,
    pub streaming: bool,
    pub prefetch: i64,
    pub debug_name: String,
}

fn main() -> anyhow::Result<()> {
    let want = std::env::args().nth(1).expect("usage: <event-substr>").to_ascii_lowercase();

    let usmap = Usmap::meteorite()?;
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut bytes = None;
    let mut found = String::new();
    'outer: for utoc in &utocs {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        for e in a.entries() {
            let p = e.path.replace('\\', "/").to_ascii_lowercase();
            if p.contains("/events/") && p.ends_with(".uasset") && p.contains(&want) {
                found = e.path.clone();
                bytes = a.read(&e.path).ok();
                break 'outer;
            }
        }
    }
    let bytes = bytes.ok_or_else(|| anyhow::anyhow!("no event asset matching {want:?}"))?;
    println!("asset: {found}");

    let hdr = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes), None, CV, HV, None)
        .map_err(|e| anyhow::anyhow!("zen parse: {e}"))?;
    let names = hdr.name_map.copy_raw_names();
    // Export data is addressed per-export: header_size + the export's own
    // cooked serial offset/size, not simply "everything after the header".
    let ex = hdr
        .export_map
        .first()
        .ok_or_else(|| anyhow::anyhow!("no exports"))?;
    let start = hdr.summary.header_size as usize + ex.cooked_serial_offset as usize;
    let end = start + ex.cooked_serial_size as usize;
    let body = bytes
        .get(start..end)
        .ok_or_else(|| anyhow::anyhow!("export slice {start}..{end} out of range"))?;
    println!("export: {} bytes at {start}..{end}", body.len());

    // `UAkAudioEvent`'s unversioned header is a single fragment that *skips all
    // 8* reflected properties — so `EventCookedData` is not reachable through
    // `read_export_struct("AkAudioEvent")`. The Wwise plugin instead writes the
    // struct natively right after the (empty) property block, still in
    // unversioned form. A handful of bytes separate the two, so probe for the
    // offset at which `FWwiseLocalizedEventCookedData` parses cleanly.
    let reflected = read_export_struct(body, &names, &usmap, "AkAudioEvent")
        .map_err(|e| anyhow::anyhow!("export parse: {e}"))?;
    println!("reflected properties present: {:?}", reflected.keys().collect::<Vec<_>>());

    let mut cooked = None;
    let mut at = 0usize;
    for off in 0..24.min(body.len()) {
        let r = read_export_struct(&body[off..], &names, &usmap, "WwiseLocalizedEventCookedData");
        if std::env::var("EV_DEBUG").is_ok() {
            match &r {
                Ok(v) => eprintln!(
                    "  off {off}: ok  map={:?} debug={:?} eventid={:?}",
                    v.get("EventLanguageMap").and_then(|m| m.as_map()).map(|m| m.len()),
                    v.get("DebugName").and_then(|x| x.as_str()),
                    v.get("EventId"),
                ),
                Err(e) => eprintln!("  off {off}: err {e}"),
            }
        }
        let Ok(v) = r else { continue };
        let Some(map) = v.get("EventLanguageMap").and_then(|m| m.as_map()) else { continue };
        if map.is_empty() {
            continue;
        }
        // A wrong-but-lucky start offset still decodes the nested map while
        // leaving the trailing scalars garbage. The real one has the outer
        // EventId agreeing with the per-language EventId.
        let outer = i(v.get("EventId"));
        let inner = map.first().and_then(|(_, ev)| ev.as_struct()).map(|e| i(e.get("EventId")));
        if outer != 0 && Some(outer) == inner {
            cooked = Some(v);
            at = off;
            break;
        }
    }
    let Some(cooked) = cooked else {
        anyhow::bail!("could not locate FWwiseLocalizedEventCookedData in export body");
    };
    println!("EventCookedData found at body offset {at}");
    let cooked = &cooked;
    println!("\nEventCookedData:");
    println!("  DebugName : {}", s(cooked.get("DebugName")));
    let ev_id = i(cooked.get("EventId")) as u32;
    let dbg = s(cooked.get("DebugName"));
    println!("  EventId   : {ev_id}");
    // Wwise short IDs are FNV-1 32 over the lowercased name — an independent
    // check that we decoded the right field rather than a plausible neighbour.
    let fnv = blam_tags::audio::wwise::hash_name(&dbg);
    println!("  FNV(name) : {fnv}  {}", if fnv == ev_id { "== MATCH" } else { "!= mismatch" });

    let Some(map) = cooked.get("EventLanguageMap").and_then(|v| v.as_map()) else {
        anyhow::bail!("no EventLanguageMap");
    };

    let mut all: Vec<Media> = Vec::new();
    println!("\nEventLanguageMap ({} entries):", map.len());
    for (k, v) in map {
        let lang = k.as_struct();
        let lang_name = lang.map(|l| s(l.get("LanguageName"))).unwrap_or_default();
        let lang_id = lang.map(|l| i(l.get("LanguageId"))).unwrap_or_default();
        let Some(ev) = v.as_struct() else { continue };

        println!("\n  [{lang_name}] (LanguageId {})  EventId {}", lang_id as u32, i(ev.get("EventId")) as u32);

        if let Some(banks) = ev.get("SoundBanks").and_then(|x| x.as_array()) {
            for bk in banks {
                let Some(bk) = bk.as_struct() else { continue };
                println!(
                    "     bank  id={} path={} containsMedia={} type={:?}",
                    i(bk.get("SoundBankId")) as u32,
                    s(bk.get("SoundBankPathName")),
                    b(bk.get("bContainsMedia")),
                    bk.get("SoundBankType"),
                );
            }
        }
        if let Some(media) = ev.get("Media").and_then(|x| x.as_array()) {
            for m in media {
                let Some(m) = m.as_struct() else { continue };
                let e = Media {
                    language: lang_name.clone(),
                    media_id: i(m.get("MediaId")),
                    path: s(m.get("MediaPathName")),
                    streaming: b(m.get("bStreaming")),
                    prefetch: i(m.get("PrefetchSize")),
                    debug_name: s(m.get("DebugName")),
                };
                println!(
                    "     media id={} path={} streaming={} prefetch={} debug={}",
                    e.media_id, e.path, e.streaming, e.prefetch, e.debug_name
                );
                all.push(e);
            }
        }
        if let Some(ext) = ev.get("ExternalSources").and_then(|x| x.as_array()) {
            for x in ext {
                let Some(x) = x.as_struct() else { continue };
                println!(
                    "     extsrc cookie={} debug={}",
                    i(x.get("Cookie")),
                    s(x.get("DebugName"))
                );
            }
        }
        if let Some(leaves) = ev.get("SwitchContainerLeaves").and_then(|x| x.as_array()) {
            if !leaves.is_empty() {
                println!("     switchContainerLeaves: {}", leaves.len());
            }
        }
    }

    let mut by_lang: BTreeMap<String, usize> = BTreeMap::new();
    for m in &all {
        *by_lang.entry(m.language.clone()).or_default() += 1;
    }
    println!("\ntotal media {} across {} language(s): {by_lang:?}", all.len(), by_lang.len());
    Ok(())
}
