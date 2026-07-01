//! Wwise (Audiokinetic) audio support — Halo 4 / H2A.
//!
//! Unlike Halo 3+ (FMOD FSB5, see the parent module), Halo 4 uses the Wwise
//! engine: `.sound` tags carry only a Wwise *event name*, and the real audio
//! lives in `<game>/sound/pc/*.pck` (`AKPK` packages). Resolving a tag to PCM
//! is a multi-step chain:
//!
//! ```text
//!   tag Event Name  --FNV1 hash-->  HIRC Event
//!                   --> Action(Play) --> Sound object --> source id
//!                   --> .wem bytes (embedded in a bank's DATA, or streamed)
//!                   --> Wwise-Vorbis / PCM decode --> DecodedPcm
//! ```
//!
//! This module builds bottom-up, one piece per submodule:
//! - [`pck`]: AKPK `.pck` container reader.
//! - [`bnk`]: Wwise SoundBank chunk parser (`BKHD`/`DIDX`/`DATA`/`HIRC`).
//! - [`ww_vorbis`]: Wwise-Vorbis → standard Ogg reconstruction (ww2ogg port).
//! - [`wem`]: `.wem` RIFF parse + decode to PCM.
//! - [`fnv`]: Wwise name hashing (event name → id).
//! - [`hirc`]: HIRC event-graph parse + event → media-source resolution.

pub mod banks;
pub mod bnk;
pub mod fnv;
pub mod hirc;
pub mod pck;
pub mod wem;
pub mod ww_vorbis;

pub use banks::WwiseBanks;
pub use bnk::{Bnk, DidxEntry};
pub use fnv::hash_name;
pub use hirc::{HircIndex, SoundSource};
pub use pck::{Pck, PckEntry};
pub use wem::decode_wem;
