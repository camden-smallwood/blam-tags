//! Decode a cooked `UAkAudioEvent` export into the Wwise media it plays.
//!
//! In a Wwise-driven UE title the audio a gameplay object triggers is not in
//! the object at all — it is an `AkAudioEvent` asset whose cooked
//! `FWwiseLocalizedEventCookedData` names, per language, the sound banks and
//! `.wem` media the event needs. That struct is what turns "this tag plays a
//! sound" into "these files, in this language".
//!
//! # Why this can't just call `read_export_struct("AkAudioEvent")`
//!
//! `UAkAudioEvent`'s unversioned property header is a single fragment that
//! *skips every one of its reflected properties*, so a reflection-driven read
//! correctly yields nothing. The plugin serializes `EventCookedData` natively,
//! immediately after that empty property block — still in unversioned form, so
//! the nested structs decode normally once the stream is positioned on them.
//! A few bytes separate the two, and the reader transparently walks the zero
//! fragments in between, so [`read_event_cooked_data`] probes a small window of
//! start offsets and accepts the first that is self-consistent.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};

use super::unversioned::{read_export_struct, PropValue};
use super::usmap::Usmap;

/// How far into the export body to look for the natively-written struct.
const PROBE_WINDOW: usize = 24;

fn as_int(v: Option<&PropValue>) -> i64 {
    match v {
        Some(PropValue::Int(n)) => *n,
        _ => 0,
    }
}

fn as_string(v: Option<&PropValue>) -> String {
    v.and_then(PropValue::as_str).unwrap_or_default().to_string()
}

fn as_bool(v: Option<&PropValue>) -> bool {
    matches!(v, Some(PropValue::Bool(true)))
}

/// One `.wem` an event references, together with the language it belongs to.
#[derive(Debug, Clone)]
pub struct MediaEntry {
    /// Wwise language name, e.g. `SFX` (non-localized) or `English(US)`.
    pub language: String,
    /// Wwise short id; also the media's file stem.
    pub media_id: u32,
    /// Container-relative media path, e.g. `Media/43/43030714.wem`, or
    /// `Media/English(US)/66/662006976.wem` when localized.
    pub path: String,
    /// Whether the media streams rather than living in the bank.
    pub streaming: bool,
    /// The source `.wav` this was authored from — the closest thing to a
    /// human-readable permutation name.
    pub source_name: String,
}

/// One sound bank an event depends on.
#[derive(Debug, Clone)]
pub struct BankEntry {
    pub language: String,
    pub bank_id: u32,
    /// Container-relative bank path, e.g. `English(US)/84450521.bnk`.
    pub path: String,
    pub contains_media: bool,
}

/// A decoded `FWwiseLocalizedEventCookedData`.
#[derive(Debug, Clone, Default)]
pub struct EventCookedData {
    /// The authored Wwise event name (`DebugName`), e.g. `Play_Foo_Bar`.
    pub event_name: String,
    /// The Wwise short id — the FNV-1 hash of [`event_name`](Self::event_name).
    pub event_id: u32,
    /// Every language the event was cooked for, in map order.
    pub languages: Vec<String>,
    pub media: Vec<MediaEntry>,
    pub banks: Vec<BankEntry>,
}

impl EventCookedData {
    /// Media for one language name (case-insensitive). Non-localized events
    /// carry a single `SFX` language.
    pub fn media_for_language(&self, language: &str) -> Vec<&MediaEntry> {
        self.media.iter().filter(|m| m.language.eq_ignore_ascii_case(language)).collect()
    }

    /// Whether the event is non-localized (its only language is `SFX`).
    pub fn is_sfx(&self) -> bool {
        self.languages.len() == 1 && self.languages[0].eq_ignore_ascii_case("SFX")
    }
}

/// Pull the cooked data out of an `AkAudioEvent` export body.
///
/// `export` must be the export's own serial range (`header_size +
/// cooked_serial_offset`, length `cooked_serial_size`) and `names` the owning
/// package's name map.
pub fn read_event_cooked_data(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
) -> Result<EventCookedData> {
    let props = locate_cooked_struct(export, names, usmap)
        .context("no FWwiseLocalizedEventCookedData in AkAudioEvent export")?;

    let mut out = EventCookedData {
        event_name: as_string(props.get("DebugName")),
        event_id: as_int(props.get("EventId")) as u32,
        ..Default::default()
    };

    let Some(map) = props.get("EventLanguageMap").and_then(PropValue::as_map) else {
        bail!("EventCookedData has no EventLanguageMap");
    };

    for (key, value) in map {
        let language = key
            .as_struct()
            .map(|l| as_string(l.get("LanguageName")))
            .unwrap_or_default();
        out.languages.push(language.clone());

        let Some(event) = value.as_struct() else { continue };

        if let Some(banks) = event.get("SoundBanks").and_then(PropValue::as_array) {
            for bank in banks.iter().filter_map(PropValue::as_struct) {
                out.banks.push(BankEntry {
                    language: language.clone(),
                    bank_id: as_int(bank.get("SoundBankId")) as u32,
                    path: as_string(bank.get("SoundBankPathName")),
                    contains_media: as_bool(bank.get("bContainsMedia")),
                });
            }
        }
        if let Some(media) = event.get("Media").and_then(PropValue::as_array) {
            for m in media.iter().filter_map(PropValue::as_struct) {
                out.media.push(MediaEntry {
                    language: language.clone(),
                    media_id: as_int(m.get("MediaId")) as u32,
                    path: as_string(m.get("MediaPathName")),
                    streaming: as_bool(m.get("bStreaming")),
                    source_name: as_string(m.get("DebugName")),
                });
            }
        }
    }
    Ok(out)
}

/// Find the offset at which the natively-written struct starts.
///
/// A wrong-but-lucky offset still decodes the nested map while leaving the
/// trailing scalars reading from the wrong place, so require the outer
/// `EventId` to agree with the first language's — a shifted stream won't.
fn locate_cooked_struct(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
) -> Option<BTreeMap<String, PropValue>> {
    for off in 0..PROBE_WINDOW.min(export.len()) {
        let Ok(props) =
            read_export_struct(&export[off..], names, usmap, "WwiseLocalizedEventCookedData")
        else {
            continue;
        };
        let Some(map) = props.get("EventLanguageMap").and_then(PropValue::as_map) else {
            continue;
        };
        if map.is_empty() {
            continue;
        }
        let outer = as_int(props.get("EventId"));
        let inner = map.first().and_then(|(_, e)| e.as_struct()).map(|e| as_int(e.get("EventId")));
        if outer != 0 && Some(outer) == inner {
            return Some(props);
        }
    }
    None
}
