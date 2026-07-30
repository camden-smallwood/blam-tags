//! Open the PC Wwise packages beside the tags tree (`<game>/sound/pc/`) and
//! resolve `.sound` tag event names to decoded PCM.
//!
//! This is the Wwise counterpart to the FMOD [`super::super::SoundBanks`]. It
//! opens every `.pck`, builds a global HIRC index plus a `source id → .wem
//! location` map (embedded in a bank's `DATA`, or streamed), and turns an event
//! name into a [`DecodedPcm`]. Bank/media bytes are read lazily — the stream
//! package is ~1.7 GB — so only the directories are held in memory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::super::vorbis::DecodedPcm;
use super::bnk::Bnk;
use super::hirc::HircIndex;
use super::pck::Pck;
use super::wem::decode_wem;

/// Where a `.wem`'s bytes live: which opened `.pck` and the byte range.
#[derive(Clone, Copy)]
struct MediaLoc {
    pck: usize,
    offset: u64,
    size: u64,
}

pub struct WwiseBanks {
    pcks: Vec<Pck>,
    hirc: HircIndex,
    /// source id → embedded media (in a bank `DATA` chunk).
    embedded: HashMap<u32, MediaLoc>,
    /// source id → streamed media (loose `.wem` in the stream package).
    streamed: HashMap<u32, MediaLoc>,
}

impl WwiseBanks {
    /// Localized languages available beside the tags tree: every subfolder of
    /// `<game>/sound/pc/` that contains a `.pck`, sorted. Cheap (directory scan).
    pub fn available_languages(tags_root: &Path) -> Vec<String> {
        let base = tags_root
            .parent()
            .unwrap_or(tags_root)
            .join("sound")
            .join("pc");
        let mut langs = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                let p = entry.path();
                let has_pck = p.is_dir()
                    && std::fs::read_dir(&p)
                        .map(|rd| {
                            rd.flatten()
                                .any(|e| e.path().extension().is_some_and(|x| x == "pck"))
                        })
                        .unwrap_or(false);
                if has_pck {
                    if let Some(name) = p.file_name() {
                        langs.push(name.to_string_lossy().into_owned());
                    }
                }
            }
        }
        langs.sort();
        langs
    }

    /// Open every `.pck` under `<game>/sound/pc/` and build the resolution index
    /// (default dialogue language). Errs if the directory has no readable packages.
    pub fn open_pc(tags_root: &Path) -> Result<Self, String> {
        Self::open_pc_language(tags_root, None)
    }

    /// Like [`Self::open_pc`] but opens the given dialogue-language subfolder's
    /// packages (plus the shared top-level SFX packages). `language = None` uses
    /// `PREFERRED_LANGUAGE`.
    pub fn open_pc_language(tags_root: &Path, language: Option<&str>) -> Result<Self, String> {
        let base = tags_root
            .parent()
            .unwrap_or(tags_root)
            .join("sound")
            .join("pc");
        // Shared SFX packages live at the top level (`sfxbank.pck` = event graph
        // + embedded media; `sfxstream.pck` = streamed media). Dialogue/campaign
        // events live in the per-language subfolders — but all six languages
        // carry an *identical* event graph referencing language-specific media,
        // so opening more than one adds no events, only redundant reads. Pick
        // one language (English by default) for the dialogue banks.
        let mut paths: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&base) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().is_some_and(|e| e == "pck") {
                    paths.push(p); // sfxbank.pck / sfxstream.pck (+ DLC stubs, skipped)
                }
            }
        }
        if let Some(lang) = pick_language_dir(&base, language) {
            if let Ok(rd) = std::fs::read_dir(&lang) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if p.extension().is_some_and(|e| e == "pck") {
                        paths.push(p); // <lang>/soundbank.pck + soundstream.pck
                    }
                }
            }
        }
        paths.sort(); // deterministic order across platforms

        let mut banks = WwiseBanks {
            pcks: Vec::new(),
            hirc: HircIndex::new(),
            embedded: HashMap::new(),
            streamed: HashMap::new(),
        };
        for path in paths {
            let Ok(pck) = Pck::open(&path) else { continue };
            let pck_index = banks.pcks.len();
            // Streamed media: record locations from the stream table.
            for entry in &pck.streams {
                banks.streamed.entry(entry.id).or_insert(MediaLoc {
                    pck: pck_index,
                    offset: entry.offset,
                    size: entry.size,
                });
            }
            // Bank media + event graph: read each bank once, extract HIRC and
            // embedded-media locations, then drop the bytes.
            for entry in &pck.banks {
                let Ok(bytes) = pck.read_entry(entry) else {
                    continue;
                };
                let Ok(bnk) = Bnk::parse(bytes) else { continue };
                for (id, off_in_bank, size) in bnk.embedded_locations() {
                    banks.embedded.entry(id).or_insert(MediaLoc {
                        pck: pck_index,
                        offset: entry.offset + off_in_bank as u64,
                        size: size as u64,
                    });
                }
                if let Some(hirc) = bnk.hirc_bytes() {
                    banks.hirc.add_hirc(hirc, bnk.version);
                }
            }
            banks.pcks.push(pck);
        }

        if banks.pcks.is_empty() {
            return Err(format!("no Wwise .pck under {}", base.display()));
        }
        banks.hirc.finalize();
        Ok(banks)
    }

    /// Read the raw `.wem` bytes for a resolved source id, if known.
    fn read_source(&self, source_id: u32, streamed: bool) -> Option<Vec<u8>> {
        let loc = if streamed {
            self.streamed
                .get(&source_id)
                .or_else(|| self.embedded.get(&source_id))
        } else {
            self.embedded
                .get(&source_id)
                .or_else(|| self.streamed.get(&source_id))
        }?;
        self.pcks[loc.pck].read_range(loc.offset, loc.size).ok()
    }

    /// True if an event name resolves to at least one known media source.
    pub fn has_event(&self, event_name: &str) -> bool {
        self.hirc
            .resolve_event(event_name)
            .iter()
            .any(|s| self.embedded.contains_key(&s.source_id) || self.streamed.contains_key(&s.source_id))
    }

    /// Resolve an event name and decode the first playable source to PCM.
    /// (An event may map to several sources — e.g. a random container; the
    /// first that decodes to audio is returned.)
    pub fn resolve(&self, event_name: &str) -> Result<DecodedPcm, String> {
        let sources = self.hirc.resolve_event(event_name);
        if sources.is_empty() {
            return Err(format!("event '{event_name}' not found in Wwise banks"));
        }
        let mut last_err = None;
        let mut empty: Option<DecodedPcm> = None;
        for src in sources {
            let Some(bytes) = self.read_source(src.source_id, src.streamed) else {
                last_err = Some(format!("source {} bytes not found", src.source_id));
                continue;
            };
            match decode_wem(&bytes) {
                Ok(pcm) if pcm.frame_count() > 0 => return Ok(pcm),
                Ok(pcm) => empty = empty.or(Some(pcm)),
                Err(e) => last_err = Some(e),
            }
        }
        // All sources were ultra-short/empty: return the (silent) first, else err.
        empty.ok_or_else(|| last_err.unwrap_or_else(|| "no decodable source".into()))
    }
}

/// Preferred dialogue language (all languages share one event graph, so this
/// only selects which localized audio plays).
const PREFERRED_LANGUAGE: &str = "english(us)";

/// Choose the language subfolder under `sound/pc/` whose dialogue banks to open:
/// the explicit `language` if given and present, else [`PREFERRED_LANGUAGE`],
/// else the first subfolder that contains a `.pck`. `None` if there are no
/// language subfolders (e.g. SFX-only installs).
fn pick_language_dir(base: &Path, language: Option<&str>) -> Option<PathBuf> {
    if let Some(lang) = language {
        let explicit = base.join(lang);
        if explicit.is_dir() {
            return Some(explicit);
        }
    }
    let preferred = base.join(PREFERRED_LANGUAGE);
    if preferred.is_dir() {
        return Some(preferred);
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(base)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && std::fs::read_dir(p)
                    .map(|rd| {
                        rd.flatten()
                            .any(|e| e.path().extension().is_some_and(|x| x == "pck"))
                    })
                    .unwrap_or(false)
        })
        .collect();
    dirs.sort();
    dirs.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end resolve against a real install. Set `WWISE_TAGS_ROOT` to a
    /// `<game>/tags` dir (its sibling `sound/pc/*.pck` are opened).
    #[test]
    #[ignore]
    fn resolves_event_to_pcm() {
        let Some(root) = std::env::var_os("WWISE_TAGS_ROOT").map(PathBuf::from) else {
            eprintln!("WWISE_TAGS_ROOT unset; skipping");
            return;
        };
        let banks = WwiseBanks::open_pc(&root).expect("open wwise banks");
        let event =
            std::env::var("WWISE_EVENT").unwrap_or_else(|_| "play_m30_a_60_sfx".to_owned());
        let pcm = banks.resolve(&event).expect("resolve event");
        eprintln!(
            "play_m30_a_60_sfx -> {} ch, {} Hz, {} frames ({:.2}s)",
            pcm.channels,
            pcm.sample_rate,
            pcm.frame_count(),
            pcm.duration_secs()
        );
        assert!(pcm.frame_count() > 0);
    }

    /// In-engine coverage report (skip-if-absent): walk every `.sound` tag,
    /// read its `Event Name`, and count how many resolve against the banks.
    /// Absences are expected — campaign/DLC events live in on-demand banks not
    /// present in the flat `sound/pc` install. `WWISE_TAGS_ROOT=<game>/tags`.
    #[test]
    #[ignore]
    fn event_coverage_report() {
        let Some(root) = std::env::var_os("WWISE_TAGS_ROOT").map(PathBuf::from) else {
            eprintln!("WWISE_TAGS_ROOT unset; skipping");
            return;
        };
        let banks = WwiseBanks::open_pc(&root).expect("open wwise banks");

        // Recursively collect .sound tags under <root>/sound.
        let mut tags = Vec::new();
        collect_sounds(&root.join("sound"), &mut tags);

        let (mut with_event, mut resolved) = (0usize, 0usize);
        for path in &tags {
            let Ok(tag) = crate::TagFile::read(path) else {
                continue;
            };
            let tag_root = tag.root();
            let Some(field) = tag_root
                .fields()
                .map(|f| f.name().to_owned())
                .find(|n| n.eq_ignore_ascii_case("event name"))
            else {
                continue;
            };
            let Some(event) = tag_root.read_string_id(&field) else {
                continue;
            };
            with_event += 1;
            if banks.has_event(&event) {
                resolved += 1;
            }
        }
        eprintln!(
            "H4 event coverage: {resolved}/{with_event} tags resolve ({} total .sound tags)",
            tags.len()
        );
        assert!(resolved > 0);
    }

    fn collect_sounds(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_sounds(&p, out);
            } else if p.extension().is_some_and(|e| e == "sound") {
                out.push(p);
            }
        }
    }
}
