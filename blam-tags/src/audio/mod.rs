//! FMOD FSB5 sound-bank parsing + FMOD-Vorbis decode for Halo 3+ audio.
//!
//! Halo 3 and later store their runtime audio as FMOD-Vorbis subsounds in
//! `<game>/fmod/pc/*.fsb` — the compiled `.sound` tags themselves carry only
//! zeroed placeholder sample buffers (the real data is paged out to these
//! banks). This module:
//!
//! - parses the FSB5 container ([`fsb5`]),
//! - decodes FMOD-Vorbis subsounds to interleaved PCM ([`vorbis`]) using a
//!   codebook table compiled into the binary (`vorbis_books.bin`), and
//! - resolves `.sound` tag paths / permutation names to subsounds ([`banks`]).
//!
//! Audio *output* is intentionally not handled here — this module only turns
//! bank bytes into [`DecodedPcm`]; the caller drives playback. Everything is
//! pure Rust (only `lewton`), gated behind the `audio` cargo feature.

pub mod banks;
pub mod classic;
pub mod fsb5;
pub mod vorbis;
pub mod wwise;

pub use banks::SoundBanks;
pub use wwise::WwiseBanks;
pub use classic::{
    decode_opus, decode_pcm, decode_xbox_adpcm, encode_opus, encode_pcm, encode_xbox_adpcm,
};
pub use fsb5::{
    fmod_bank_subsound_id_hash, fmod_pitch_range_folder, sound_leaf_name, Fsb5, SubSound,
};
pub use vorbis::{
    decode_ogg_vorbis, decode_subsound, downmix_to_stereo, encode_ogg_vorbis, DecodedPcm,
};

use std::path::{Path, PathBuf};

/// Resolve the primary FMOD SFX bank path from a tags root: the banks live
/// beside the `tags/` tree at `<game>/fmod/pc/sfx.fsb`. Prefer
/// [`SoundBanks::open_pc`] to open every bank (SFX + per-language dialogue).
pub fn bank_path_from_tags_root(tags_root: &Path) -> PathBuf {
    tags_root
        .parent()
        .unwrap_or(tags_root)
        .join("fmod")
        .join("pc")
        .join("sfx.fsb")
}
