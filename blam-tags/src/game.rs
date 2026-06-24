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

/// A specific MCC title / loaded editing kit, identified by its game-id
/// (the kit folder name, e.g. `"halo4_mcc"`).
///
/// This is a *different axis* from [`Game`]: [`Game`] is the tag-format
/// generation derived from the tag bytes, and H3, ODST, Reach, and H4 all share
/// `Game::Halo3` because their `render_model`/JMS/ASS structures are identical.
/// The title captures which of those engines is actually loaded — a distinction
/// the tag bytes don't encode, but that matters for engine details like the HDR
/// tonemap (H3/Reach use a cubic curve, H4 a Hable filmic). It comes from the
/// loading context (the editing kit), not the tag, so it's parsed from the
/// game-id string rather than `of(tag)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Title {
    HaloCe,
    Halo2,
    Halo2A,
    Halo3,
    Halo3Odst,
    HaloReach,
    Halo4,
}

impl Title {
    /// Parse an MCC game-id / editing-kit folder name (e.g. `"halo4_mcc"`).
    /// Returns `None` for an unrecognized id.
    pub fn from_game_id(id: &str) -> Option<Title> {
        Some(match id {
            "haloce_mcc" => Title::HaloCe,
            "halo2_mcc" => Title::Halo2,
            "halo2amp_mcc" => Title::Halo2A,
            "halo3_mcc" => Title::Halo3,
            "halo3odst_mcc" => Title::Halo3Odst,
            "haloreach_mcc" => Title::HaloReach,
            "halo4_mcc" => Title::Halo4,
            _ => return None,
        })
    }
}

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
    /// read/write.
    ///
    /// Unlike JMS/ASS, all three generations share version **16392** —
    /// the `header + node{name,first_child,next_sibling} + per-frame
    /// transforms` layout. HABT (`io_scene_halo/file_jma`) lists 16392 as
    /// valid for CE/H2/H3 (`__init__.py`: `16390` = "CE/H2/H3"), and our
    /// writer already emits 16392 with H3+Reach corpus validation. The
    /// later `16395` H2/H3 variant only adds an optional biped-controller
    /// transform block, which extraction doesn't need; keeping one version
    /// per game here gives a single dispatch point should that change.
    pub fn jma_version(self) -> u16 {
        match self {
            Game::Halo1 | Game::Halo2 | Game::Halo3 => 16392,
        }
    }
}
