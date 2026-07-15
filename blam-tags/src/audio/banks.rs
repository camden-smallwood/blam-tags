//! Open the PC FMOD banks beside the tags tree (`<game>/fmod/pc/`).
//!
//! Campaign dialogue is usually in `english.fsb` (copy from MCC per
//! `fmod/pc/README_LANGUAGES.txt`); music/foley/SFX live in `sfx.fsb`.

use std::path::{Path, PathBuf};

use super::fsb5::Fsb5;

pub struct SoundBanks {
    banks: Vec<Fsb5>,
    paths: Vec<PathBuf>,
}

/// The `<game>/fmod/pc/` directory beside a tags tree.
fn fmod_pc_dir(tags_root: &Path) -> PathBuf {
    tags_root
        .parent()
        .unwrap_or(tags_root)
        .join("fmod")
        .join("pc")
}

/// Non-localized bank name (shared across languages).
const SFX_BANK: &str = "sfx.fsb";

impl SoundBanks {
    /// Localized languages available beside the tags tree: every `<lang>.fsb`
    /// under `<game>/fmod/pc/` except the shared `sfx.fsb`, sorted. Cheap (a
    /// directory scan; no bank is opened).
    pub fn available_languages(tags_root: &Path) -> Vec<String> {
        let base = fmod_pc_dir(tags_root);
        let mut langs = Vec::new();
        let Ok(entries) = std::fs::read_dir(&base) else {
            return langs;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".fsb") {
                if !name.eq_ignore_ascii_case(SFX_BANK) {
                    langs.push(stem.to_owned());
                }
            }
        }
        langs.sort();
        langs
    }

    /// Open the default banks (`english.fsb` + `dialogue.fsb` + `sfx.fsb`).
    pub fn open_pc(tags_root: &Path) -> Result<Self, String> {
        Self::open_pc_language(tags_root, None)
    }

    /// Open the banks for a specific language: `<language>.fsb` (localized
    /// dialogue/music) plus the shared `sfx.fsb`, the language bank taking
    /// resolve priority. `language = None` opens the default set
    /// (`english.fsb` + `dialogue.fsb` + `sfx.fsb`).
    pub fn open_pc_language(tags_root: &Path, language: Option<&str>) -> Result<Self, String> {
        let base = fmod_pc_dir(tags_root);
        // Language bank first so it wins over sfx on a name clash.
        let candidates: Vec<String> = match language {
            Some(lang) => vec![format!("{lang}.fsb"), SFX_BANK.to_owned()],
            None => vec![
                "english.fsb".to_owned(),
                "dialogue.fsb".to_owned(),
                SFX_BANK.to_owned(),
            ],
        };
        let mut banks = Vec::new();
        let mut paths = Vec::new();
        for name in &candidates {
            let p = base.join(name);
            if !p.exists() {
                continue;
            }
            let bank = Fsb5::open(&p)?;
            if !bank.is_vorbis() {
                log::warn!("[audio] {} is not FMOD-Vorbis; skipped", p.display());
                continue;
            }
            paths.push(p);
            banks.push(bank);
        }
        if banks.is_empty() {
            return Err(format!("no FMOD banks under {}", base.display()));
        }
        log::info!(
            "[audio] opened {} bank(s): {}",
            banks.len(),
            paths
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy())
                .collect::<Vec<_>>()
                .join(", ")
        );
        Ok(Self { banks, paths })
    }

    pub fn bank_paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Find a subsound for `sound_rel` (tag path) in the first bank that
    /// contains it. Returns `(bank_index, subsound_index)`.
    pub fn resolve(&self, sound_rel: &str) -> Option<(usize, usize)> {
        for (bi, bank) in self.banks.iter().enumerate() {
            if let Some(si) = bank.resolve_sound(sound_rel) {
                return Some((bi, si));
            }
        }
        None
    }

    /// Resolve a permutation by its `fmod bank subsound id hash` — the engine's
    /// collision-free key (see [`super::fmod_bank_subsound_id_hash`]). Returns
    /// `(bank_index, subsound_index)` from the first bank carrying that id.
    pub fn resolve_by_id(&self, id: u32) -> Option<(usize, usize)> {
        for (bi, bank) in self.banks.iter().enumerate() {
            if let Some(si) = bank.index_of_id(id) {
                return Some((bi, si));
            }
        }
        None
    }

    pub fn bank(&self, index: usize) -> &Fsb5 {
        &self.banks[index]
    }
}
