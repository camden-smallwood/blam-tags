//! Engine/game identity for a tag and the asset-format versions each
//! game uses.
//!
//! This is the single dispatch point for engine-specific asset
//! extraction: it decides which tag-structure reader to use and which
//! JMS/ASS text-format version to emit. The classic engines (Halo CE,
//! Halo 2) carry their own older render/collision tag structures and
//! older JMS/ASS versions; the gen3+ MCC engines (Halo 3, ODST, Reach,
//! Halo 4, H2A) share one structure and one version pair.

use crate::classic::ClassicEngine;
use crate::file::TagFile;

/// The Halo game (engine generation) a tag belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Game {
    /// Halo 1 / Combat Evolved (Anniversary). `gbxmodel` render geometry,
    /// JMS version 8200, no ASS (its BSP source is also JMS).
    Halo1,
    /// Halo 2. `render_model` (section-based) geometry, JMS version 8210,
    /// ASS version 2.
    Halo2,
    /// Halo 3 and the later gen3/gen4 MCC engines (ODST, Reach, Halo 4,
    /// H2A) — they share `render_model` (per-mesh-temporary) geometry,
    /// JMS version 8213, and ASS version 7.
    Halo3,
}

/// Which game a tag set belongs to, identified by its game id: the editing
/// kit folder name (`"halo4_mcc"`), or `"haloce_evolved"` for Halo: Campaign
/// Evolved.
///
/// This is a *different axis* from [`Game`]: [`Game`] is the tag-format
/// generation derived from the tag bytes, and H3, ODST, Reach, H4 and H2A all
/// share `Game::Halo3` because their `render_model`/JMS/ASS structures are
/// identical. The game id captures which of those engines a tag set is for —
/// a distinction the tag bytes don't encode, but that matters for engine
/// details like the HDR tonemap (H3/Reach use a cubic curve, H4 a Hable
/// filmic). It comes from the loading context (the editing kit or the
/// installed game), not the tag, so it's parsed from the game-id string
/// rather than `of(tag)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GameId {
    /// Halo: Combat Evolved in MCC (`haloce_mcc`): classic big-endian tags.
    HaloCe,
    /// Halo 2 in MCC (`halo2_mcc`): classic tags.
    Halo2,
    /// Halo 2 Anniversary Multiplayer (`halo2amp_mcc`).
    Halo2Amp,
    /// Halo 3 (`halo3_mcc`).
    Halo3,
    /// Halo 3: ODST (`halo3odst_mcc`).
    Halo3Odst,
    /// Halo: Reach (`haloreach_mcc`).
    HaloReach,
    /// Halo 4 (`halo4_mcc`).
    Halo4,
    /// Halo: Campaign Evolved (`haloce_evolved`): Reach-format tags, little
    /// endian, stored in Unreal IoStore containers. Not Halo CE in MCC.
    CampaignEvolved,
}

impl GameId {
    /// Every game, in release order (Campaign Evolved last).
    pub const ALL: [GameId; 8] = [
        GameId::HaloCe,
        GameId::Halo2,
        GameId::Halo2Amp,
        GameId::Halo3,
        GameId::Halo3Odst,
        GameId::HaloReach,
        GameId::Halo4,
        GameId::CampaignEvolved,
    ];

    /// Parse a game id (`"halo4_mcc"`, `"haloce_evolved"`). Exact and
    /// case-sensitive, as ids are written; `None` for anything else.
    pub fn from_id(id: &str) -> Option<GameId> {
        GameId::ALL.into_iter().find(|game| game.as_str() == id)
    }

    /// The game id, as [`GameId::from_id`] reads it.
    pub fn as_str(self) -> &'static str {
        match self {
            GameId::HaloCe => "haloce_mcc",
            GameId::Halo2 => "halo2_mcc",
            GameId::Halo2Amp => "halo2amp_mcc",
            GameId::Halo3 => "halo3_mcc",
            GameId::Halo3Odst => "halo3odst_mcc",
            GameId::HaloReach => "haloreach_mcc",
            GameId::Halo4 => "halo4_mcc",
            GameId::CampaignEvolved => "haloce_evolved",
        }
    }

    /// The tag-format generation this game's tags use.
    pub fn generation(self) -> Game {
        match self {
            GameId::HaloCe => Game::Halo1,
            GameId::Halo2 => Game::Halo2,
            _ => Game::Halo3,
        }
    }

    /// Whether this game's tags are classic (Halo CE or Halo 2 in MCC):
    /// headers and layouts that predate the self-describing MCC format.
    pub fn is_classic(self) -> bool {
        matches!(self, GameId::HaloCe | GameId::Halo2)
    }

    /// Whether this is Halo: Campaign Evolved.
    pub fn is_campaign_evolved(self) -> bool {
        self == GameId::CampaignEvolved
    }
}

impl std::fmt::Display for GameId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for GameId {
    type Err = UnknownGameId;

    fn from_str(id: &str) -> Result<GameId, UnknownGameId> {
        GameId::from_id(id).ok_or_else(|| UnknownGameId(id.to_owned()))
    }
}

/// A game id that [`GameId::from_id`] does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownGameId(pub String);

impl std::fmt::Display for UnknownGameId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown game id `{}`", self.0)
    }
}

impl std::error::Error for UnknownGameId {}

impl Game {
    /// Classify a tag by its container engine: classic Halo CE → Halo1,
    /// classic Halo 2 (any sub-version) → Halo2, MCC self-describing →
    /// Halo3.
    pub fn of(tag: &TagFile) -> Game {
        match tag.classic_engine() {
            Some(ClassicEngine::HaloCe) => Game::Halo1,
            Some(_) => Game::Halo2,
            None => Game::Halo3,
        }
    }

    /// The JMS text-format version this game's tools read/write.
    pub fn jms_version(self) -> u16 {
        match self {
            Game::Halo1 => 8200,
            Game::Halo2 => 8210,
            Game::Halo3 => 8213,
        }
    }

    /// The ASS text-format version, or `None` for Halo 1 (no ASS format —
    /// Halo 1 BSP source is JMS).
    pub fn ass_version(self) -> Option<u16> {
        match self {
            Game::Halo1 => None,
            Game::Halo2 => Some(2),
            Game::Halo3 => Some(7),
        }
    }

    /// The JMA-family (animation) text-format version this game's tools
    /// read/write, as their MCC tool.exe readers accept it:
    ///
    /// - **Halo 1: 16392.** Halo CE tool.exe parses 16390–16393 only, and
    ///   writes 16392 itself.
    /// - **Halo 2 and Halo 3+: 16394.** Both parse 16390–16395, but Halo 2's
    ///   refuses anything below 16394 ("ANIMATION FILE IS OUTDATED! …
    ///   expected at least version 16394") and Halo 3's warns that the import
    ///   "may have problems later".
    ///
    /// 16394 is a different layout, not a renumbering; see
    /// [`crate::animation::JMA_ABSOLUTE_VERSION`]. The later 16395 only adds
    /// an optional per-frame root transform block, which extraction does not
    /// need.
    pub fn jma_version(self) -> u16 {
        match self {
            Game::Halo1 => 16392,
            Game::Halo2 | Game::Halo3 => crate::animation::JMA_ABSOLUTE_VERSION,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Game, GameId};

    #[test]
    fn every_game_id_reads_back_as_itself() {
        for game in GameId::ALL {
            assert_eq!(GameId::from_id(game.as_str()), Some(game));
            assert_eq!(game.to_string().parse::<GameId>(), Ok(game));
        }
        let ids: std::collections::HashSet<_> = GameId::ALL.iter().map(|g| g.as_str()).collect();
        assert_eq!(ids.len(), GameId::ALL.len());
    }

    #[test]
    fn an_unknown_or_differently_cased_id_is_refused() {
        assert_eq!(GameId::from_id("halo5_mcc"), None);
        assert_eq!(GameId::from_id("HALO3_MCC"), None);
        assert_eq!(GameId::from_id(""), None);
        assert!("halo5_mcc".parse::<GameId>().is_err());
    }

    #[test]
    fn only_halo_ce_and_halo_2_are_classic() {
        let classic: Vec<_> = GameId::ALL.into_iter().filter(|g| g.is_classic()).collect();
        assert_eq!(classic, [GameId::HaloCe, GameId::Halo2]);
        assert_eq!(GameId::HaloCe.generation(), Game::Halo1);
        assert_eq!(GameId::Halo2.generation(), Game::Halo2);
        assert_eq!(GameId::CampaignEvolved.generation(), Game::Halo3);
        assert!(GameId::CampaignEvolved.is_campaign_evolved());
        assert!(!GameId::HaloCe.is_campaign_evolved());
    }
}
