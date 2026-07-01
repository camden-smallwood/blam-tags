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

impl SoundBanks {
    /// Open every `.fsb` present under `<game>/fmod/pc/`, preferring
    /// dialogue banks before `sfx.fsb` when resolving tag paths.
    pub fn open_pc(tags_root: &Path) -> Result<Self, String> {
        let base = tags_root
            .parent()
            .unwrap_or(tags_root)
            .join("fmod")
            .join("pc");
        let candidates = ["english.fsb", "dialogue.fsb", "sfx.fsb"];
        let mut banks = Vec::new();
        let mut paths = Vec::new();
        for name in candidates {
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

    pub fn bank(&self, index: usize) -> &Fsb5 {
        &self.banks[index]
    }
}
