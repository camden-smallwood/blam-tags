//! Schema-faithful walker for the `multiplayer_globals` (mulg) tag.
//!
//! Mirrors `definitions/halo3_mcc/multiplayer_globals.json` 1:1: every
//! block/struct/field maps onto the public type tree below with the same
//! nesting. Type names are PascalCase with the `_block`/`_struct`/
//! `_definition` suffixes stripped; field names are the snake_case of the
//! schema field's canonical base name (the text before the first `^ : # | !`
//! annotation). `[explanation]`/`[pad]`/`[terminator]` fields carry no data
//! and are skipped.
//!
//! The eleven per-gametype game-engine event blocks
//! (`game_engine_general_event_block` … `game_engine_gun_game_event_block`)
//! are structurally identical — 25 fields each, differing only in the
//! `event^` enum's option list — so they are modelled by the single generic
//! [`GameEngineEvent<E>`], parameterised by the per-gametype event enum.

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::typed_enums::{Enum, Flags};

//================================================================================
// Typed enums / flags (one per `enums_flags` definition in the schema).
//================================================================================

/// `game_engine_event_flags_definition` (word_flags).
#[derive(Clone, Copy, PartialEq, Eq, Debug, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum GameEngineEventFlags {
    #[strum(serialize = "quantity message")] QuantityMessage = 0,
    #[strum(serialize = "suppress text")] SuppressText = 1,
}

/// `game_engine_sound_response_flags_definition` (word_flags).
#[derive(Clone, Copy, PartialEq, Eq, Debug, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum GameEngineSoundResponseFlags {
    #[strum(serialize = "announcer sound")] AnnouncerSound = 0,
}

/// `game_engine_general_event_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineGeneralEvent {
    #[default] #[strum(serialize = "kill")] Kill = 0,
    #[strum(serialize = "suicide")] Suicide = 1,
    #[strum(serialize = "kill teammate")] KillTeammate = 2,
    #[strum(serialize = "victory")] Victory = 3,
    #[strum(serialize = "team victory")] TeamVictory = 4,
    #[strum(serialize = "unused1")] Unused1 = 5,
    #[strum(serialize = "unused2")] Unused2 = 6,
    #[strum(serialize = "1 min to win")] N1MinToWin = 7,
    #[strum(serialize = "team 1 min to win")] Team1MinToWin = 8,
    #[strum(serialize = "30 secs to win")] N30SecsToWin = 9,
    #[strum(serialize = "team 30 secs to win")] Team30SecsToWin = 10,
    #[strum(serialize = "player quit")] PlayerQuit = 11,
    #[strum(serialize = "player joined")] PlayerJoined = 12,
    #[strum(serialize = "killed by unknown")] KilledByUnknown = 13,
    #[strum(serialize = "30 minutes left")] N30MinutesLeft = 14,
    #[strum(serialize = "15 minutes left")] N15MinutesLeft = 15,
    #[strum(serialize = "5 minutes left")] N5MinutesLeft = 16,
    #[strum(serialize = "1 minute left")] N1MinuteLeft = 17,
    #[strum(serialize = "time expired")] TimeExpired = 18,
    #[strum(serialize = "game over")] GameOver = 19,
    #[strum(serialize = "respawn tick")] RespawnTick = 20,
    #[strum(serialize = "last respawn tick")] LastRespawnTick = 21,
    #[strum(serialize = "teleporter used")] TeleporterUsed = 22,
    #[strum(serialize = "teleporter blocked")] TeleporterBlocked = 23,
    #[strum(serialize = "player changed team")] PlayerChangedTeam = 24,
    #[strum(serialize = "player rejoined")] PlayerRejoined = 25,
    #[strum(serialize = "gained lead")] GainedLead = 26,
    #[strum(serialize = "gained team lead")] GainedTeamLead = 27,
    #[strum(serialize = "lost lead")] LostLead = 28,
    #[strum(serialize = "lost team lead")] LostTeamLead = 29,
    #[strum(serialize = "tied leader")] TiedLeader = 30,
    #[strum(serialize = "tied team leader")] TiedTeamLeader = 31,
    #[strum(serialize = "round over")] RoundOver = 32,
    #[strum(serialize = "30 seconds left")] N30SecondsLeft = 33,
    #[strum(serialize = "10 seconds left")] N10SecondsLeft = 34,
    #[strum(serialize = "kill (falling)")] KillFalling = 35,
    #[strum(serialize = "kill (collision)")] KillCollision = 36,
    #[strum(serialize = "kill (melee)")] KillMelee = 37,
    #[strum(serialize = "sudden death")] SuddenDeath = 38,
    #[strum(serialize = "player booted player")] PlayerBootedPlayer = 39,
    #[strum(serialize = "kill (flag carrier)")] KillFlagCarrier = 40,
    #[strum(serialize = "kill (bomb carrier)")] KillBombCarrier = 41,
    #[strum(serialize = "kill (sticky grenade)")] KillStickyGrenade = 42,
    #[strum(serialize = "kill (sniper)")] KillSniper = 43,
    #[strum(serialize = "kill (st. melee)")] KillStMelee = 44,
    #[strum(serialize = "boarded vehicle")] BoardedVehicle = 45,
    #[strum(serialize = "start team noti.")] StartTeamNoti = 46,
    #[strum(serialize = "telefrag")] Telefrag = 47,
    #[strum(serialize = "10 secs to win")] N10SecsToWin = 48,
    #[strum(serialize = "team 10 secs to win")] Team10SecsToWin = 49,
    #[strum(serialize = "bulltrue")] Bulltrue = 50,
    #[strum(serialize = "death from the grave")] DeathFromTheGrave = 51,
    #[strum(serialize = "hijack")] Hijack = 52,
    #[strum(serialize = "skyjack")] Skyjack = 53,
    #[strum(serialize = "kill (spartan laser)")] KillSpartanLaser = 54,
    #[strum(serialize = "kill (flame)")] KillFlame = 55,
    #[strum(serialize = "assist (to driver)")] AssistToDriver = 56,
    #[strum(serialize = "assist")] Assist = 57,
    #[strum(serialize = "pre game over")] PreGameOver = 58,
}

/// `game_engine_event_audience_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineEventAudience {
    #[default] #[strum(serialize = "cause player")] CausePlayer = 0,
    #[strum(serialize = "cause team")] CauseTeam = 1,
    #[strum(serialize = "effect player")] EffectPlayer = 2,
    #[strum(serialize = "effect team")] EffectTeam = 3,
    #[strum(serialize = "all")] All = 4,
}

/// `game_engine_event_response_context_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineEventResponseContext {
    #[default] #[strum(serialize = "self")] SelfVariant = 0,
    #[strum(serialize = "friendly")] Friendly = 1,
    #[strum(serialize = "enemy")] Enemy = 2,
    #[strum(serialize = "neutral")] Neutral = 3,
}

/// `game_engine_event_input_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineEventInput {
    #[default] #[strum(serialize = "NONE")] NONE = 0,
    #[strum(serialize = "cause player")] CausePlayer = 1,
    #[strum(serialize = "cause team")] CauseTeam = 2,
    #[strum(serialize = "effect player")] EffectPlayer = 3,
    #[strum(serialize = "effect team")] EffectTeam = 4,
}

/// `game_engine_event_splitscreen_suppression_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineEventSplitscreenSuppression {
    #[default] #[strum(serialize = "NONE")] NONE = 0,
    #[strum(serialize = "Suppress Audio")] SuppressAudio = 1,
    #[strum(serialize = "Suppress Audio if Overlapping")] SuppressAudioIfOverlapping = 2,
    #[strum(serialize = "Suppress Text")] SuppressText = 3,
    #[strum(serialize = "Suppress Audio and Text")] SuppressAudioAndText = 4,
}

/// `game_engine_flavor_event_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineFlavorEvent {
    #[default] #[strum(serialize = "extermination")] Extermination = 0,
    #[strum(serialize = "perfection")] Perfection = 1,
    #[strum(serialize = "multikill x2")] MultikillX2 = 2,
    #[strum(serialize = "multikill x3")] MultikillX3 = 3,
    #[strum(serialize = "multikill x4")] MultikillX4 = 4,
    #[strum(serialize = "multikill x5")] MultikillX5 = 5,
    #[strum(serialize = "multikill x6")] MultikillX6 = 6,
    #[strum(serialize = "multikill x7")] MultikillX7 = 7,
    #[strum(serialize = "multikill x8")] MultikillX8 = 8,
    #[strum(serialize = "multikill x9")] MultikillX9 = 9,
    #[strum(serialize = "multikill x10")] MultikillX10 = 10,
    #[strum(serialize = "5 in a row")] N5InARow = 11,
    #[strum(serialize = "10 in a row")] N10InARow = 12,
    #[strum(serialize = "15 in a row")] N15InARow = 13,
    #[strum(serialize = "20 in a row")] N20InARow = 14,
    #[strum(serialize = "25 in a row")] N25InARow = 15,
    #[strum(serialize = "30 in a row")] N30InARow = 16,
    #[strum(serialize = "sniper 5x")] Sniper5x = 17,
    #[strum(serialize = "sniper 10x")] Sniper10x = 18,
    #[strum(serialize = "shotgun 5x")] Shotgun5x = 19,
    #[strum(serialize = "shotgun 10x")] Shotgun10x = 20,
    #[strum(serialize = "splatter 5x")] Splatter5x = 21,
    #[strum(serialize = "splatter 10x")] Splatter10x = 22,
    #[strum(serialize = "sword 5x")] Sword5x = 23,
    #[strum(serialize = "sword 10x")] Sword10x = 24,
    #[strum(serialize = "juggernaut kill 5x")] JuggernautKill5x = 25,
    #[strum(serialize = "juggernaut kill 10x")] JuggernautKill10x = 26,
    #[strum(serialize = "zombie kill 5x")] ZombieKill5x = 27,
    #[strum(serialize = "zombie kill 10x")] ZombieKill10x = 28,
    #[strum(serialize = "survivor kill x5")] SurvivorKillX5 = 29,
    #[strum(serialize = "survivor kill x10")] SurvivorKillX10 = 30,
    #[strum(serialize = "survivor kill x15")] SurvivorKillX15 = 31,
    #[strum(serialize = "hill kill 5x")] HillKill5x = 32,
    #[strum(serialize = "bulltrue")] Bulltrue = 33,
    #[strum(serialize = "splatter")] Splatter = 34,
    #[strum(serialize = "hijack")] Hijack = 35,
    #[strum(serialize = "skyjack")] Skyjack = 36,
    #[strum(serialize = "from the grave")] FromTheGrave = 37,
    #[strum(serialize = "broke killing spree")] BrokeKillingSpree = 38,
    #[strum(serialize = "laser kill")] LaserKill = 39,
    #[strum(serialize = "stickygrenade kill")] StickygrenadeKill = 40,
    #[strum(serialize = "sniper kill")] SniperKill = 41,
    #[strum(serialize = "bashbehind kill")] BashbehindKill = 42,
    #[strum(serialize = "bash kill")] BashKill = 43,
    #[strum(serialize = "flame kill")] FlameKill = 44,
    #[strum(serialize = "driver assist")] DriverAssist = 45,
    #[strum(serialize = "bomb planted")] BombPlanted = 46,
    #[strum(serialize = "bomb carrier kill")] BombCarrierKill = 47,
    #[strum(serialize = "kill vip")] KillVip = 48,
    #[strum(serialize = "kill juggernaut")] KillJuggernaut = 49,
    #[strum(serialize = "oddball carrier kill")] OddballCarrierKill = 50,
    #[strum(serialize = "flag capture")] FlagCapture = 51,
    #[strum(serialize = "kill flag carrier")] KillFlagCarrier = 52,
    #[strum(serialize = "flag carrier kill")] FlagCarrierKill = 53,
    #[strum(serialize = "survivor")] Survivor = 54,
    #[strum(serialize = "forge cant place")] ForgeCantPlace = 55,
    #[strum(serialize = "forge limit reached")] ForgeLimitReached = 56,
    #[strum(serialize = "triple double")] TripleDouble = 57,
    #[strum(serialize = "steaktacular")] Steaktacular = 58,
    #[strum(serialize = "35 in a row")] N35InARow = 59,
    #[strum(serialize = "40 in a row")] N40InARow = 60,
    #[strum(serialize = "brute shot kill")] BruteShotKill = 61,
    #[strum(serialize = "fuel rod cannon kill")] FuelRodCannonKill = 62,
    #[strum(serialize = "grenade kill")] GrenadeKill = 63,
    #[strum(serialize = "gravity hammer kill")] GravityHammerKill = 64,
    #[strum(serialize = "plasma grenade kill")] PlasmaGrenadeKill = 65,
    #[strum(serialize = "rocket launcher kill")] RocketLauncherKill = 66,
    #[strum(serialize = "needler kill")] NeedlerKill = 67,
    #[strum(serialize = "sentinel beam kill")] SentinelBeamKill = 68,
    #[strum(serialize = "sword kill")] SwordKill = 69,
    #[strum(serialize = "turret kill")] TurretKill = 70,
    #[strum(serialize = "comeback kill")] ComebackKill = 71,
    #[strum(serialize = "headshot")] Headshot = 72,
    #[strum(serialize = "generic kill")] GenericKill = 73,
    #[strum(serialize = "vehicle kill")] VehicleKill = 74,
    #[strum(serialize = "environmental kill")] EnvironmentalKill = 75,
    #[strum(serialize = "first strike")] FirstStrike = 76,
    #[strum(serialize = "last strike")] LastStrike = 77,
    #[strum(serialize = "protector")] Protector = 78,
    #[strum(serialize = "avenger")] Avenger = 79,
    #[strum(serialize = "close call")] CloseCall = 80,
    #[strum(serialize = "reload this")] ReloadThis = 81,
    #[strum(serialize = "revenge")] Revenge = 82,
    #[strum(serialize = "snapshot")] Snapshot = 83,
    #[strum(serialize = "vehicle destroyed")] VehicleDestroyed = 84,
    #[strum(serialize = "assist")] Assist = 85,
    #[strum(serialize = "vehicle destroy assist")] VehicleDestroyAssist = 86,
    #[strum(serialize = "ball hold 10s")] BallHold10s = 87,
    #[strum(serialize = "ball hold 20s")] BallHold20s = 88,
    #[strum(serialize = "ball hold 30s")] BallHold30s = 89,
    #[strum(serialize = "ball hold 45s")] BallHold45s = 90,
    #[strum(serialize = "ball hold 60s")] BallHold60s = 91,
    #[strum(serialize = "ball multikill x3")] BallMultikillX3 = 92,
    #[strum(serialize = "ball carrier kill")] BallCarrierKill = 93,
    #[strum(serialize = "ball first touch")] BallFirstTouch = 94,
    #[strum(serialize = "hill first point")] HillFirstPoint = 95,
    #[strum(serialize = "hill defense")] HillDefense = 96,
    #[strum(serialize = "hill offense")] HillOffense = 97,
    #[strum(serialize = "hill dominance")] HillDominance = 98,
    #[strum(serialize = "flag taken")] FlagTaken = 99,
    #[strum(serialize = "flag returned")] FlagReturned = 100,
    #[strum(serialize = "flag runner")] FlagRunner = 101,
    #[strum(serialize = "flag champion")] FlagChampion = 102,
    #[strum(serialize = "bomb defense")] BombDefense = 103,
    #[strum(serialize = "bomb defused")] BombDefused = 104,
    #[strum(serialize = "disarmed")] Disarmed = 105,
    #[strum(serialize = "saboteur")] Saboteur = 106,
    #[strum(serialize = "territory captured")] TerritoryCaptured = 107,
    #[strum(serialize = "territorial")] Territorial = 108,
    #[strum(serialize = "triumvirate")] Triumvirate = 109,
}

/// `game_engine_slayer_event_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineSlayerEvent {
    #[default] #[strum(serialize = "game start")] GameStart = 0,
    #[strum(serialize = "new target")] NewTarget = 1,
    #[strum(serialize = "pre game start")] PreGameStart = 2,
}

/// `game_engine_ctf_event_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineCtfEvent {
    #[default] #[strum(serialize = "game start")] GameStart = 0,
    #[strum(serialize = "flag taken")] FlagTaken = 1,
    #[strum(serialize = "flag grabbed from stand")] FlagGrabbedFromStand = 2,
    #[strum(serialize = "flag dropped")] FlagDropped = 3,
    #[strum(serialize = "flag returned by player")] FlagReturnedByPlayer = 4,
    #[strum(serialize = "flag returned by timeout")] FlagReturnedByTimeout = 5,
    #[strum(serialize = "flag captured")] FlagCaptured = 6,
    #[strum(serialize = "flag new defensive team")] FlagNewDefensiveTeam = 7,
    #[strum(serialize = "flag return faliure")] FlagReturnFaliure = 8,
    #[strum(serialize = "side switch tick")] SideSwitchTick = 9,
    #[strum(serialize = "side switch final tick")] SideSwitchFinalTick = 10,
    #[strum(serialize = "side switch 30 seconds")] SideSwitch30Seconds = 11,
    #[strum(serialize = "side switch 10 seconds")] SideSwitch10Seconds = 12,
    #[strum(serialize = "flag contested")] FlagContested = 13,
    #[strum(serialize = "flag capture faliure")] FlagCaptureFaliure = 14,
}

/// `game_engine_oddball_event_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineOddballEvent {
    #[default] #[strum(serialize = "game start")] GameStart = 0,
    #[strum(serialize = "ball spawned")] BallSpawned = 1,
    #[strum(serialize = "ball picked up")] BallPickedUp = 2,
    #[strum(serialize = "ball dropped")] BallDropped = 3,
    #[strum(serialize = "ball reset")] BallReset = 4,
    #[strum(serialize = "ball tick")] BallTick = 5,
    #[strum(serialize = "10 points remaining")] N10PointsRemaining = 6,
    #[strum(serialize = "25 points remaining")] N25PointsRemaining = 7,
    #[strum(serialize = "50 points remaining")] N50PointsRemaining = 8,
}

/// `game_engine_king_event_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineKingEvent {
    #[default] #[strum(serialize = "game start")] GameStart = 0,
    #[strum(serialize = "hill controlled")] HillControlled = 1,
    #[strum(serialize = "hill contested")] HillContested = 2,
    #[strum(serialize = "hill tick")] HillTick = 3,
    #[strum(serialize = "hill move")] HillMove = 4,
    #[strum(serialize = "hill controlled team")] HillControlledTeam = 5,
    #[strum(serialize = "hill contested team")] HillContestedTeam = 6,
}

/// `game_engine_vip_event_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineVipEvent {
    #[default] #[strum(serialize = "game start")] GameStart = 0,
    #[strum(serialize = "new vip")] NewVip = 1,
    #[strum(serialize = "vip killed")] VipKilled = 2,
    #[strum(serialize = "vip died")] VipDied = 3,
    #[strum(serialize = "side switch")] SideSwitch = 4,
    #[strum(serialize = "arrive at destination")] ArriveAtDestination = 5,
    #[strum(serialize = "destination moved")] DestinationMoved = 6,
}

/// `game_engine_juggernaut_event_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineJuggernautEvent {
    #[default] #[strum(serialize = "game start")] GameStart = 0,
    #[strum(serialize = "new juggernaut")] NewJuggernaut = 1,
    #[strum(serialize = "zone moved")] ZoneMoved = 2,
    #[strum(serialize = "kill (of juggernaut)")] KillOfJuggernaut = 3,
}

/// `game_engine_territories_event_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineTerritoriesEvent {
    #[default] #[strum(serialize = "game start")] GameStart = 0,
    #[strum(serialize = "territory captured")] TerritoryCaptured = 1,
    #[strum(serialize = "territory lost")] TerritoryLost = 2,
    #[strum(serialize = "territory locked")] TerritoryLocked = 3,
    #[strum(serialize = "side switch")] SideSwitch = 4,
}

/// `game_engine_assault_event_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineAssaultEvent {
    #[default] #[strum(serialize = "game start")] GameStart = 0,
    #[strum(serialize = "bomb taken")] BombTaken = 1,
    #[strum(serialize = "bomb dropped")] BombDropped = 2,
    #[strum(serialize = "bomb returned by player")] BombReturnedByPlayer = 3,
    #[strum(serialize = "bomb returned by timeout")] BombReturnedByTimeout = 4,
    #[strum(serialize = "bomb captured")] BombCaptured = 5,
    #[strum(serialize = "bomb new defensive team")] BombNewDefensiveTeam = 6,
    #[strum(serialize = "bomb return faliure")] BombReturnFaliure = 7,
    #[strum(serialize = "side switch tick")] SideSwitchTick = 8,
    #[strum(serialize = "side switch final tick")] SideSwitchFinalTick = 9,
    #[strum(serialize = "side switch 30 seconds")] SideSwitch30Seconds = 10,
    #[strum(serialize = "side switch 10 seconds")] SideSwitch10Seconds = 11,
    #[strum(serialize = "bomb returned by defusing")] BombReturnedByDefusing = 12,
    #[strum(serialize = "bomb placed on enemy post")] BombPlacedOnEnemyPost = 13,
    #[strum(serialize = "bomb arming started")] BombArmingStarted = 14,
    #[strum(serialize = "bomb arming completed")] BombArmingCompleted = 15,
    #[strum(serialize = "bomb contested")] BombContested = 16,
    #[strum(serialize = "neutral bomb returned")] NeutralBombReturned = 17,
}

/// `game_engine_infection_event_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineInfectionEvent {
    #[default] #[strum(serialize = "game start")] GameStart = 0,
    #[strum(serialize = "new infection")] NewInfection = 1,
    #[strum(serialize = "zombie spawn")] ZombieSpawn = 2,
    #[strum(serialize = "alpha zombie spawn")] AlphaZombieSpawn = 3,
    #[strum(serialize = "player survived")] PlayerSurvived = 4,
}

/// `game_engine_gun_game_event_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineGunGameEvent {
    #[default] #[strum(serialize = "game start")] GameStart = 0,
    #[strum(serialize = "player tier upgraded")] PlayerTierUpgraded = 1,
    #[strum(serialize = "player tier downgraded")] PlayerTierDowngraded = 2,
    #[strum(serialize = "player at final tier")] PlayerAtFinalTier = 3,
}

/// `game_engine_status_enum_definition` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GameEngineStatus {
    #[default] #[strum(serialize = "waiting for space to clear")] WaitingForSpaceToClear = 0,
    #[strum(serialize = "observing")] Observing = 1,
    #[strum(serialize = "respawning soon")] RespawningSoon = 2,
    #[strum(serialize = "sitting out")] SittingOut = 3,
    #[strum(serialize = "out of lives")] OutOfLives = 4,
    #[strum(serialize = "playing (winning)")] PlayingWinning = 5,
    #[strum(serialize = "playing (tied)")] PlayingTied = 6,
    #[strum(serialize = "playing (losing)")] PlayingLosing = 7,
    #[strum(serialize = "game over (won)")] GameOverWon = 8,
    #[strum(serialize = "game over (tied)")] GameOverTied = 9,
    #[strum(serialize = "game over (lost)")] GameOverLost = 10,
    #[strum(serialize = "game over (you lost, but game tied)")] GameOverYouLostButGameTied = 11,
    #[strum(serialize = "you have flag")] YouHaveFlag = 12,
    #[strum(serialize = "enemy has flag")] EnemyHasFlag = 13,
    #[strum(serialize = "flag not home")] FlagNotHome = 14,
    #[strum(serialize = "carrying oddball")] CarryingOddball = 15,
    #[strum(serialize = "you are juggy")] YouAreJuggy = 16,
    #[strum(serialize = "you control hill")] YouControlHill = 17,
    #[strum(serialize = "switching sides soon")] SwitchingSidesSoon = 18,
    #[strum(serialize = "player recently started")] PlayerRecentlyStarted = 19,
    #[strum(serialize = "you have bomb")] YouHaveBomb = 20,
    #[strum(serialize = "flag contested")] FlagContested = 21,
    #[strum(serialize = "bomb contested")] BombContested = 22,
    #[strum(serialize = "limited lives left (multiple)")] LimitedLivesLeftMultiple = 23,
    #[strum(serialize = "limited lives left (single)")] LimitedLivesLeftSingle = 24,
    #[strum(serialize = "limited lives left (final)")] LimitedLivesLeftFinal = 25,
    #[strum(serialize = "playing (winning, unlimited)")] PlayingWinningUnlimited = 26,
    #[strum(serialize = "playing (tied, unlimited)")] PlayingTiedUnlimited = 27,
    #[strum(serialize = "playing (losing, unlimited)")] PlayingLosingUnlimited = 28,
}
//================================================================================
// GameEngineEvent<E> — the 11 structurally-identical game-engine event blocks.
//================================================================================

/// One `game_engine_*_event_block` element (25 data fields). All eleven
/// per-gametype event blocks share this exact layout; only the `event^`
/// enum (`E`) differs — `GameEngineGeneralEvent`, `GameEngineFlavorEvent`,
/// `GameEngineSlayerEvent`, … . The trailing `!` runtime-only annotations
/// on `runtime event type`, `display priority`, `sub priority` and
/// `probability` are dropped from the canonical read key but the fields
/// are still walked.
#[derive(Clone)]
pub struct GameEngineEvent<E> {
    pub flags: Flags<GameEngineEventFlags, u16>,
    pub runtime_event_type: i16,
    pub event: Enum<E, i16>,
    pub audience: Enum<GameEngineEventAudience, i16>,
    pub display_priority: i16,
    pub sub_priority: i16,
    pub display_context: Enum<GameEngineEventResponseContext, i16>,
    pub display_string: String,
    pub medal_award: String,
    pub required_field: Enum<GameEngineEventInput, i16>,
    pub excluded_audience: Enum<GameEngineEventInput, i16>,
    pub splitscreen_suppression: Enum<GameEngineEventSplitscreenSuppression, i16>,
    pub primary_string: String,
    pub primary_string_duration: i32,
    pub plural_display_string: String,
    pub sound_delay_announcer_only: f32,
    pub sound_flags: Flags<GameEngineSoundResponseFlags, u16>,
    pub sound: String,
    pub extra_sounds: SoundResponseExtraSounds,
    pub probability: f32,
    pub sound_permutations: Vec<SoundResponseDefinition>,
}

impl<E: crate::typed_enums::SchemaEnum + Default> GameEngineEvent<E> {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            flags: s.try_read_flags("flags").unwrap_or_default(),
            runtime_event_type: s.read_int_any("runtime event type").unwrap_or_default() as i16,
            event: s.try_read_enum("event").unwrap_or_default(),
            audience: s.try_read_enum("audience").unwrap_or_default(),
            display_priority: s.read_int_any("display priority").unwrap_or_default() as i16,
            sub_priority: s.read_int_any("sub priority").unwrap_or_default() as i16,
            display_context: s.try_read_enum("display context").unwrap_or_default(),
            display_string: s.read_string_id("display string").unwrap_or_default(),
            medal_award: s.read_string_id("medal award").unwrap_or_default(),
            required_field: s.try_read_enum("required field").unwrap_or_default(),
            excluded_audience: s.try_read_enum("excluded audience").unwrap_or_default(),
            splitscreen_suppression: s.try_read_enum("splitscreen suppression").unwrap_or_default(),
            primary_string: s.read_string_id("primary string").unwrap_or_default(),
            primary_string_duration: s.read_int_any("primary string duration").unwrap_or_default() as i32,
            plural_display_string: s.read_string_id("plural display string").unwrap_or_default(),
            sound_delay_announcer_only: s.read_real("sound delay (announcer only)").unwrap_or_default(),
            sound_flags: s.try_read_flags("sound flags").unwrap_or_default(),
            sound: s.read_tag_ref_path("sound").unwrap_or_default(),
            extra_sounds: SoundResponseExtraSounds::from_struct(
                &s.field("extra sounds").and_then(|f| f.as_struct()).unwrap_or_else(|| s.clone()),
            ),
            probability: s.read_real("probability").unwrap_or_default(),
            sound_permutations: read_block_vec(s, "sound permutations", SoundResponseDefinition::from_struct),
        }
    }
}

impl<E> std::fmt::Debug for GameEngineEvent<E>
where
    E: num_traits::FromPrimitive + num_traits::ToPrimitive + Copy + std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameEngineEvent")
            .field("event", &self.event)
            .field("audience", &self.audience)
            .field("flags", &self.flags)
            .field("display_string", &self.display_string)
            .field("sound", &self.sound)
            .field("sound_permutations", &self.sound_permutations.len())
            .finish_non_exhaustive()
    }
}

//================================================================================
// Schema-faithful sub-structs (mirrors multiplayer_globals.json 1:1).
//================================================================================

/// `multiplayer_color_block`.
#[derive(Debug, Clone, Default)]
pub struct MultiplayerColor {
    pub color: crate::math::RealRgbColor,
}

impl MultiplayerColor {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            color: s.read_rgb("color"),
        }
    }
}

/// `customized_model_bits_block`.
#[derive(Debug, Clone, Default)]
pub struct CustomizedModelBit {
    pub region_name: String,
    pub permutation_name: String,
}

impl CustomizedModelBit {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            region_name: s.read_string_id("region name").unwrap_or_default(),
            permutation_name: s.read_string_id("permutation name").unwrap_or_default(),
        }
    }
}

/// `customized_model_selection_block`.
#[derive(Debug, Clone, Default)]
pub struct CustomizedModelSelection {
    pub selection_name: String,
    pub selection_description: String,
    pub customized_bits: Vec<CustomizedModelBit>,
}

impl CustomizedModelSelection {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            selection_name: s.read_string_id("selection name").unwrap_or_default(),
            selection_description: s.read_string_id("selection description").unwrap_or_default(),
            customized_bits: read_block_vec(s, "customized bits", CustomizedModelBit::from_struct),
        }
    }
}

/// `customized_model_area_block`.
#[derive(Debug, Clone, Default)]
pub struct CustomizedModelArea {
    pub area_name: String,
    pub customized_selection: Vec<CustomizedModelSelection>,
}

impl CustomizedModelArea {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            area_name: s.read_string_id("area name").unwrap_or_default(),
            customized_selection: read_block_vec(s, "customized selection", CustomizedModelSelection::from_struct),
        }
    }
}

/// `customized_model_characters_block`.
#[derive(Debug, Clone, Default)]
pub struct CustomizedModelCharacter {
    pub character_name: String,
    pub customized_areas: Vec<CustomizedModelArea>,
}

impl CustomizedModelCharacter {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            character_name: s.read_string_id("character name").unwrap_or_default(),
            customized_areas: read_block_vec(s, "customized areas", CustomizedModelArea::from_struct),
        }
    }
}

/// `weapon_selections_block`.
#[derive(Debug, Clone, Default)]
pub struct WeaponSelection {
    pub name: String,
    pub random_weapon_set_weight: f32,
    pub weapon_tag: String,
}

impl WeaponSelection {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            random_weapon_set_weight: s.read_real("random weapon set weight").unwrap_or_default(),
            weapon_tag: s.read_tag_ref_path("weapon tag").unwrap_or_default(),
        }
    }
}

/// `vehicle_selections_block`.
#[derive(Debug, Clone, Default)]
pub struct VehicleSelection {
    pub name: String,
    pub vehicle_tag: String,
}

impl VehicleSelection {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            vehicle_tag: s.read_tag_ref_path("vehicle tag").unwrap_or_default(),
        }
    }
}

/// `grenade_selections_block`.
#[derive(Debug, Clone, Default)]
pub struct GrenadeSelection {
    pub name: String,
    pub grenade_tag: String,
}

impl GrenadeSelection {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            grenade_tag: s.read_tag_ref_path("grenade tag").unwrap_or_default(),
        }
    }
}

/// `equipment_selection_block`.
#[derive(Debug, Clone, Default)]
pub struct EquipmentSelection {
    pub name: String,
    pub random_weight: f32,
    pub equipment_tag: String,
}

impl EquipmentSelection {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            random_weight: s.read_real("random weight").unwrap_or_default(),
            equipment_tag: s.read_tag_ref_path("equipment tag").unwrap_or_default(),
        }
    }
}

/// `object_remap_entry_block`.
#[derive(Debug, Clone, Default)]
pub struct ObjectRemapEntry {
    pub placed_object_name: String,
    pub remapped_object_name: String,
}

impl ObjectRemapEntry {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            placed_object_name: s.read_string_id("placed object name").unwrap_or_default(),
            remapped_object_name: s.read_string_id("remapped object name").unwrap_or_default(),
        }
    }
}

/// `multiplayer_weapon_set_block`.
#[derive(Debug, Clone, Default)]
pub struct MultiplayerWeaponSet {
    pub name: String,
    pub remap_table: Vec<ObjectRemapEntry>,
}

impl MultiplayerWeaponSet {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            remap_table: read_block_vec(s, "remap table", ObjectRemapEntry::from_struct),
        }
    }
}

/// `multiplayer_vehicle_set_block`.
#[derive(Debug, Clone, Default)]
pub struct MultiplayerVehicleSet {
    pub name: String,
    pub remap_table: Vec<ObjectRemapEntry>,
}

impl MultiplayerVehicleSet {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            remap_table: read_block_vec(s, "remap table", ObjectRemapEntry::from_struct),
        }
    }
}

/// `multiplayer_universal_block`.
#[derive(Debug, Clone, Default)]
pub struct UniversalGlobals {
    pub multiplayer_zone_required_resources: String,
    pub random_player_names: String,
    pub team_names: String,
    pub team_colors: Vec<MultiplayerColor>,
    pub customized_characters: Vec<CustomizedModelCharacter>,
    pub multiplayer_text: String,
    pub sandbox_text: String,
    pub sandbox_object_properties_values: String,
    pub weapon_selections: Vec<WeaponSelection>,
    pub vehicle_selections: Vec<VehicleSelection>,
    pub grenade_selections: Vec<GrenadeSelection>,
    pub equipment_selections: Vec<EquipmentSelection>,
    pub weapon_sets: Vec<MultiplayerWeaponSet>,
    pub vehicle_sets: Vec<MultiplayerVehicleSet>,
    pub halo3_game_engine_settings: String,
}

impl UniversalGlobals {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            multiplayer_zone_required_resources: s.read_tag_ref_path("multiplayer zone required resources").unwrap_or_default(),
            random_player_names: s.read_tag_ref_path("random player names").unwrap_or_default(),
            team_names: s.read_tag_ref_path("team names").unwrap_or_default(),
            team_colors: read_block_vec(s, "team colors", MultiplayerColor::from_struct),
            customized_characters: read_block_vec(s, "customized characters", CustomizedModelCharacter::from_struct),
            multiplayer_text: s.read_tag_ref_path("multiplayer text").unwrap_or_default(),
            sandbox_text: s.read_tag_ref_path("sandbox text").unwrap_or_default(),
            sandbox_object_properties_values: s.read_tag_ref_path("sandbox object properties values").unwrap_or_default(),
            weapon_selections: read_block_vec(s, "weapon selections", WeaponSelection::from_struct),
            vehicle_selections: read_block_vec(s, "vehicle selections", VehicleSelection::from_struct),
            grenade_selections: read_block_vec(s, "grenade selections", GrenadeSelection::from_struct),
            equipment_selections: read_block_vec(s, "equipment selections", EquipmentSelection::from_struct),
            weapon_sets: read_block_vec(s, "weapon sets", MultiplayerWeaponSet::from_struct),
            vehicle_sets: read_block_vec(s, "vehicle sets", MultiplayerVehicleSet::from_struct),
            halo3_game_engine_settings: s.read_tag_ref_path("halo3 game engine settings").unwrap_or_default(),
        }
    }
}

/// `sounds_block`.
#[derive(Debug, Clone, Default)]
pub struct Sound {
    pub sound: String,
}

impl Sound {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            sound: s.read_tag_ref_path("sound").unwrap_or_default(),
        }
    }
}

/// `looping_sounds_block`.
#[derive(Debug, Clone, Default)]
pub struct LoopingSound {
    pub looping_sound: String,
}

impl LoopingSound {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            looping_sound: s.read_tag_ref_path("looping sound").unwrap_or_default(),
        }
    }
}

/// `sound_response_extra_sounds_struct`.
#[derive(Debug, Clone, Default)]
pub struct SoundResponseExtraSounds {
    pub japanese_sound: String,
    pub german_sound: String,
    pub french_sound: String,
    pub spanish_sound: String,
    pub mexican_sound: String,
    pub italian_sound: String,
    pub korean_sound: String,
    pub chinese_sound_traditional: String,
    pub chinese_sound_simplified: String,
    pub portuguese_sound: String,
    pub polish_sound: String,
}

impl SoundResponseExtraSounds {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            japanese_sound: s.read_tag_ref_path("japanese sound").unwrap_or_default(),
            german_sound: s.read_tag_ref_path("german sound").unwrap_or_default(),
            french_sound: s.read_tag_ref_path("french sound").unwrap_or_default(),
            spanish_sound: s.read_tag_ref_path("spanish sound").unwrap_or_default(),
            mexican_sound: s.read_tag_ref_path("mexican sound").unwrap_or_default(),
            italian_sound: s.read_tag_ref_path("italian sound").unwrap_or_default(),
            korean_sound: s.read_tag_ref_path("korean sound").unwrap_or_default(),
            chinese_sound_traditional: s.read_tag_ref_path("chinese sound (traditional)").unwrap_or_default(),
            chinese_sound_simplified: s.read_tag_ref_path("chinese sound (simplified)").unwrap_or_default(),
            portuguese_sound: s.read_tag_ref_path("portuguese sound").unwrap_or_default(),
            polish_sound: s.read_tag_ref_path("polish sound").unwrap_or_default(),
        }
    }
}

/// `sound_response_definition_block`.
#[derive(Debug, Clone, Default)]
pub struct SoundResponseDefinition {
    pub sound_flags: Flags<GameEngineSoundResponseFlags, u16>,
    pub english_sound: String,
    pub extra_sounds: SoundResponseExtraSounds,
    pub probability: f32,
}

impl SoundResponseDefinition {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            sound_flags: s.try_read_flags("sound flags").unwrap_or_default(),
            english_sound: s.read_tag_ref_path("english sound").unwrap_or_default(),
            extra_sounds: SoundResponseExtraSounds::from_struct(&s.field("extra sounds").and_then(|f| f.as_struct()).unwrap_or_else(|| s.clone())),
            probability: s.read_real("probability").unwrap_or_default(),
        }
    }
}

/// `weapon_spawn_influence_block`.
#[derive(Debug, Clone, Default)]
pub struct WeaponSpawnInfluence {
    pub weapon: String,
    pub full_weight_range: f32,
    pub fall_off_range: f32,
    pub fall_off_cone_radius: f32,
    pub weight: f32,
}

impl WeaponSpawnInfluence {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            weapon: s.read_tag_ref_path("weapon").unwrap_or_default(),
            full_weight_range: s.read_real("full weight range").unwrap_or_default(),
            fall_off_range: s.read_real("fall-off range").unwrap_or_default(),
            fall_off_cone_radius: s.read_real("fall-off cone radius").unwrap_or_default(),
            weight: s.read_real("weight").unwrap_or_default(),
        }
    }
}

/// `vehicle_spawn_influence_block`.
#[derive(Debug, Clone, Default)]
pub struct VehicleSpawnInfluence {
    pub vehicle: String,
    pub pill_radius: f32,
    pub lead_time: f32,
    pub minimum_velocity: f32,
    pub weight: f32,
}

impl VehicleSpawnInfluence {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            vehicle: s.read_tag_ref_path("vehicle").unwrap_or_default(),
            pill_radius: s.read_real("pill radius").unwrap_or_default(),
            lead_time: s.read_real("lead time").unwrap_or_default(),
            minimum_velocity: s.read_real("minimum velocity").unwrap_or_default(),
            weight: s.read_real("weight").unwrap_or_default(),
        }
    }
}

/// `projectile_spawn_influence_block`.
#[derive(Debug, Clone, Default)]
pub struct ProjectileSpawnInfluence {
    pub projectile: String,
    pub lead_time: f32,
    pub collision_cylinder_radius: f32,
    pub weight: f32,
}

impl ProjectileSpawnInfluence {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            projectile: s.read_tag_ref_path("projectile").unwrap_or_default(),
            lead_time: s.read_real("lead time").unwrap_or_default(),
            collision_cylinder_radius: s.read_real("collision cylinder radius").unwrap_or_default(),
            weight: s.read_real("weight").unwrap_or_default(),
        }
    }
}

/// `equipment_spawn_influence_block`.
#[derive(Debug, Clone, Default)]
pub struct EquipmentSpawnInfluence {
    pub equipment: String,
    pub weight: f32,
}

impl EquipmentSpawnInfluence {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            equipment: s.read_tag_ref_path("equipment").unwrap_or_default(),
            weight: s.read_real("weight").unwrap_or_default(),
        }
    }
}

/// `auto_turret_spawn_influencers_block`.
#[derive(Debug, Clone, Default)]
pub struct AutoTurretSpawnInfluencers {
    pub vehicle: String,
    pub weight: f32,
}

impl AutoTurretSpawnInfluencers {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            vehicle: s.read_tag_ref_path("vehicle").unwrap_or_default(),
            weight: s.read_real("weight").unwrap_or_default(),
        }
    }
}

/// `multiplayer_constants_block`.
#[derive(Debug, Clone, Default)]
pub struct MultiplayerConstants {
    pub ef_full_weight_radius: f32,
    pub ef_fall_off_radius: f32,
    pub ef_upper_height: f32,
    pub ef_lower_height: f32,
    pub ef_weight: f32,
    pub eb_full_weight_radius: f32,
    pub eb_fall_off_radius: f32,
    pub eb_upper_height: f32,
    pub eb_lower_height: f32,
    pub eb_weight: f32,
    pub ab_full_weight_radius: f32,
    pub ab_fall_off_radius: f32,
    pub ab_upper_height: f32,
    pub ab_lower_height: f32,
    pub ab_weight: f32,
    pub sab_full_weight_radius: f32,
    pub sab_fall_off_radius: f32,
    pub sab_upper_height: f32,
    pub sab_lower_height: f32,
    pub sab_weight: f32,
    pub dt_full_weight_radius: f32,
    pub dt_fall_off_radius: f32,
    pub dt_upper_height: f32,
    pub dt_lower_height: f32,
    pub dt_weight: f32,
    pub dead_teammate_influence_duration: f32,
    pub weapon_influencers: Vec<WeaponSpawnInfluence>,
    pub vehicle_influencers: Vec<VehicleSpawnInfluence>,
    pub projectile_influencers: Vec<ProjectileSpawnInfluence>,
    pub equipment_influencers: Vec<EquipmentSpawnInfluence>,
    pub auto_turret_influencers: Vec<AutoTurretSpawnInfluencers>,
    pub koth_full_weight_radius: f32,
    pub koth_fall_off_radius: f32,
    pub koth_upper_cylinder_height: f32,
    pub koth_lower_cylinder_height: f32,
    pub koth_weight: f32,
    pub ob_full_weight_radius: f32,
    pub ob_fall_off_radius: f32,
    pub ob_upper_cylinder_height: f32,
    pub ob_lower_cylinder_height: f32,
    pub ob_weight: f32,
    pub ctf_full_weight_radius: f32,
    pub ctf_fall_off_radius: f32,
    pub ctf_upper_cylinder_height: f32,
    pub ctf_lower_cylinder_height: f32,
    pub ctf_weight: f32,
    pub tera_full_weight_radius: f32,
    pub tera_fall_off_radius: f32,
    pub tera_upper_cylinder_height: f32,
    pub tera_lower_cylinder_height: f32,
    pub tera_weight: f32,
    pub tere_full_weight_radius: f32,
    pub tere_fall_off_radius: f32,
    pub tere_upper_cylinder_height: f32,
    pub tere_lower_cylinder_height: f32,
    pub tere_weight: f32,
    pub infh_full_weight_radius: f32,
    pub infh_fall_off_radius: f32,
    pub infh_upper_cylinder_height: f32,
    pub infh_lower_cylinder_height: f32,
    pub infh_weight: f32,
    pub infz_full_weight_radius: f32,
    pub infz_fall_off_radius: f32,
    pub infz_upper_cylinder_height: f32,
    pub infz_lower_cylinder_height: f32,
    pub infz_weight: f32,
    pub vip_full_weight_radius: f32,
    pub vip_fall_off_radius: f32,
    pub vip_upper_cylinder_height: f32,
    pub vip_lower_cylinder_height: f32,
    pub vip_weight: f32,
    pub maximum_random_spawn_bias: f32,
    pub teleporter_recharge_time: f32,
    pub flag_return_distance: f32,
    pub flag_contest_inner_radius: f32,
    pub flag_contest_outer_radius: f32,
    pub territories_waypoint_vertical_offset: f32,
    pub bomb_explode_effect: String,
    pub bomb_explode_secondary_effect: String,
    pub bomb_explode_dmg_effect: String,
    pub bomb_defuse_effect: String,
    pub sandbox_effect: String,
    pub voluntary_respawn_control_instructions: String,
    pub spawn_allowed_default_respawn: String,
    pub spawn_at_player_allowed_looking_at_self: String,
    pub spawn_at_player_allowed_looking_at_target: String,
    pub spawn_at_player_allowed_looking_at_potential_target: String,
    pub spawn_at_territory_allowed_looking_at_target: String,
    pub spawn_at_territory_allowed_looking_at_potential_target: String,
    pub you_are_out_of_lives: String,
    pub invalid_spawn_target_selected: String,
    pub targetted_player_enemies_nearby: String,
    pub targetted_player_unfriendly_team: String,
    pub targetted_player_dead: String,
    pub targetted_player_in_combat: String,
    pub targetted_player_too_far_from_owned_flag: String,
    pub no_available_netpoints: String,
    pub targetted_netpoint_contested: String,
}

impl MultiplayerConstants {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            ef_full_weight_radius: s.read_real("ef full weight radius").unwrap_or_default(),
            ef_fall_off_radius: s.read_real("ef fall-off radius").unwrap_or_default(),
            ef_upper_height: s.read_real("ef upper height").unwrap_or_default(),
            ef_lower_height: s.read_real("ef lower height").unwrap_or_default(),
            ef_weight: s.read_real("ef weight").unwrap_or_default(),
            eb_full_weight_radius: s.read_real("eb full weight radius").unwrap_or_default(),
            eb_fall_off_radius: s.read_real("eb fall-off radius").unwrap_or_default(),
            eb_upper_height: s.read_real("eb upper height").unwrap_or_default(),
            eb_lower_height: s.read_real("eb lower height").unwrap_or_default(),
            eb_weight: s.read_real("eb weight").unwrap_or_default(),
            ab_full_weight_radius: s.read_real("ab full weight radius").unwrap_or_default(),
            ab_fall_off_radius: s.read_real("ab fall-off radius").unwrap_or_default(),
            ab_upper_height: s.read_real("ab upper height").unwrap_or_default(),
            ab_lower_height: s.read_real("ab lower height").unwrap_or_default(),
            ab_weight: s.read_real("ab weight").unwrap_or_default(),
            sab_full_weight_radius: s.read_real("sab full weight radius").unwrap_or_default(),
            sab_fall_off_radius: s.read_real("sab fall-off radius").unwrap_or_default(),
            sab_upper_height: s.read_real("sab upper height").unwrap_or_default(),
            sab_lower_height: s.read_real("sab lower height").unwrap_or_default(),
            sab_weight: s.read_real("sab weight").unwrap_or_default(),
            dt_full_weight_radius: s.read_real("dt full weight radius").unwrap_or_default(),
            dt_fall_off_radius: s.read_real("dt fall-off radius").unwrap_or_default(),
            dt_upper_height: s.read_real("dt upper height").unwrap_or_default(),
            dt_lower_height: s.read_real("dt lower height").unwrap_or_default(),
            dt_weight: s.read_real("dt weight").unwrap_or_default(),
            dead_teammate_influence_duration: s.read_real("dead teammate influence duration").unwrap_or_default(),
            weapon_influencers: read_block_vec(s, "weapon influencers", WeaponSpawnInfluence::from_struct),
            vehicle_influencers: read_block_vec(s, "vehicle influencers", VehicleSpawnInfluence::from_struct),
            projectile_influencers: read_block_vec(s, "projectile influencers", ProjectileSpawnInfluence::from_struct),
            equipment_influencers: read_block_vec(s, "equipment influencers", EquipmentSpawnInfluence::from_struct),
            auto_turret_influencers: read_block_vec(s, "auto turret influencers", AutoTurretSpawnInfluencers::from_struct),
            koth_full_weight_radius: s.read_real("koth full weight radius").unwrap_or_default(),
            koth_fall_off_radius: s.read_real("koth fall-off radius").unwrap_or_default(),
            koth_upper_cylinder_height: s.read_real("koth upper cylinder height").unwrap_or_default(),
            koth_lower_cylinder_height: s.read_real("koth lower cylinder height").unwrap_or_default(),
            koth_weight: s.read_real("koth weight").unwrap_or_default(),
            ob_full_weight_radius: s.read_real("ob full weight radius").unwrap_or_default(),
            ob_fall_off_radius: s.read_real("ob fall-off radius").unwrap_or_default(),
            ob_upper_cylinder_height: s.read_real("ob upper cylinder height").unwrap_or_default(),
            ob_lower_cylinder_height: s.read_real("ob lower cylinder height").unwrap_or_default(),
            ob_weight: s.read_real("ob weight").unwrap_or_default(),
            ctf_full_weight_radius: s.read_real("ctf full weight radius").unwrap_or_default(),
            ctf_fall_off_radius: s.read_real("ctf fall-off radius").unwrap_or_default(),
            ctf_upper_cylinder_height: s.read_real("ctf upper cylinder height").unwrap_or_default(),
            ctf_lower_cylinder_height: s.read_real("ctf lower cylinder height").unwrap_or_default(),
            ctf_weight: s.read_real("ctf weight").unwrap_or_default(),
            tera_full_weight_radius: s.read_real("tera full weight radius").unwrap_or_default(),
            tera_fall_off_radius: s.read_real("tera fall-off radius").unwrap_or_default(),
            tera_upper_cylinder_height: s.read_real("tera upper cylinder height").unwrap_or_default(),
            tera_lower_cylinder_height: s.read_real("tera lower cylinder height").unwrap_or_default(),
            tera_weight: s.read_real("tera weight").unwrap_or_default(),
            tere_full_weight_radius: s.read_real("tere full weight radius").unwrap_or_default(),
            tere_fall_off_radius: s.read_real("tere fall-off radius").unwrap_or_default(),
            tere_upper_cylinder_height: s.read_real("tere upper cylinder height").unwrap_or_default(),
            tere_lower_cylinder_height: s.read_real("tere lower cylinder height").unwrap_or_default(),
            tere_weight: s.read_real("tere weight").unwrap_or_default(),
            infh_full_weight_radius: s.read_real("infh full weight radius").unwrap_or_default(),
            infh_fall_off_radius: s.read_real("infh fall-off radius").unwrap_or_default(),
            infh_upper_cylinder_height: s.read_real("infh upper cylinder height").unwrap_or_default(),
            infh_lower_cylinder_height: s.read_real("infh lower cylinder height").unwrap_or_default(),
            infh_weight: s.read_real("infh weight").unwrap_or_default(),
            infz_full_weight_radius: s.read_real("infz full weight radius").unwrap_or_default(),
            infz_fall_off_radius: s.read_real("infz fall-off radius").unwrap_or_default(),
            infz_upper_cylinder_height: s.read_real("infz upper cylinder height").unwrap_or_default(),
            infz_lower_cylinder_height: s.read_real("infz lower cylinder height").unwrap_or_default(),
            infz_weight: s.read_real("infz weight").unwrap_or_default(),
            vip_full_weight_radius: s.read_real("vip full weight radius").unwrap_or_default(),
            vip_fall_off_radius: s.read_real("vip fall-off radius").unwrap_or_default(),
            vip_upper_cylinder_height: s.read_real("vip upper cylinder height").unwrap_or_default(),
            vip_lower_cylinder_height: s.read_real("vip lower cylinder height").unwrap_or_default(),
            vip_weight: s.read_real("vip weight").unwrap_or_default(),
            maximum_random_spawn_bias: s.read_real("maximum random spawn bias").unwrap_or_default(),
            teleporter_recharge_time: s.read_real("teleporter recharge time").unwrap_or_default(),
            flag_return_distance: s.read_real("flag return distance").unwrap_or_default(),
            flag_contest_inner_radius: s.read_real("flag-contest inner radius").unwrap_or_default(),
            flag_contest_outer_radius: s.read_real("flag-contest outer radius").unwrap_or_default(),
            territories_waypoint_vertical_offset: s.read_real("territories waypoint vertical offset").unwrap_or_default(),
            bomb_explode_effect: s.read_tag_ref_path("bomb explode effect").unwrap_or_default(),
            bomb_explode_secondary_effect: s.read_tag_ref_path("bomb explode secondary effect").unwrap_or_default(),
            bomb_explode_dmg_effect: s.read_tag_ref_path("bomb explode dmg effect").unwrap_or_default(),
            bomb_defuse_effect: s.read_tag_ref_path("bomb defuse effect").unwrap_or_default(),
            sandbox_effect: s.read_tag_ref_path("sandbox effect").unwrap_or_default(),
            voluntary_respawn_control_instructions: s.read_string_id("voluntary respawn control instructions").unwrap_or_default(),
            spawn_allowed_default_respawn: s.read_string_id("spawn allowed default respawn").unwrap_or_default(),
            spawn_at_player_allowed_looking_at_self: s.read_string_id("spawn at player allowed looking at self").unwrap_or_default(),
            spawn_at_player_allowed_looking_at_target: s.read_string_id("spawn at player allowed looking at target").unwrap_or_default(),
            spawn_at_player_allowed_looking_at_potential_target: s.read_string_id("spawn at player allowed looking at potential target").unwrap_or_default(),
            spawn_at_territory_allowed_looking_at_target: s.read_string_id("spawn at territory allowed looking at target").unwrap_or_default(),
            spawn_at_territory_allowed_looking_at_potential_target: s.read_string_id("spawn at territory allowed looking at potential target").unwrap_or_default(),
            you_are_out_of_lives: s.read_string_id("you are out of lives").unwrap_or_default(),
            invalid_spawn_target_selected: s.read_string_id("invalid spawn target selected").unwrap_or_default(),
            targetted_player_enemies_nearby: s.read_string_id("targetted player enemies nearby").unwrap_or_default(),
            targetted_player_unfriendly_team: s.read_string_id("targetted player unfriendly team").unwrap_or_default(),
            targetted_player_dead: s.read_string_id("targetted player dead").unwrap_or_default(),
            targetted_player_in_combat: s.read_string_id("targetted player in combat").unwrap_or_default(),
            targetted_player_too_far_from_owned_flag: s.read_string_id("targetted player too far from owned flag").unwrap_or_default(),
            no_available_netpoints: s.read_string_id("no available netpoints").unwrap_or_default(),
            targetted_netpoint_contested: s.read_string_id("targetted netpoint contested").unwrap_or_default(),
        }
    }
}

/// `game_engine_status_response_block`.
#[derive(Debug, Clone, Default)]
pub struct GameEngineStatusResponse {
    pub state: Enum<GameEngineStatus, i16>,
    pub ffa_message: String,
    pub team_message: String,
}

impl GameEngineStatusResponse {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            state: s.try_read_enum("state").unwrap_or_default(),
            ffa_message: s.read_string_id("ffa message").unwrap_or_default(),
            team_message: s.read_string_id("team message").unwrap_or_default(),
        }
    }
}

/// `multiplayer_runtime_block`.
#[derive(Debug, Clone, Default)]
pub struct RuntimeGlobals {
    pub editor_biped: String,
    pub editor_helper: String,
    pub flag: String,
    pub ball: String,
    pub assault_bomb: String,
    pub vip_influence_area: String,
    pub in_game_text: String,
    pub sounds: Vec<Sound>,
    pub looping_sounds: Vec<LoopingSound>,
    pub general_events: Vec<GameEngineEvent<GameEngineGeneralEvent>>,
    pub flavor_events: Vec<GameEngineEvent<GameEngineFlavorEvent>>,
    pub slayer_events: Vec<GameEngineEvent<GameEngineSlayerEvent>>,
    pub ctf_events: Vec<GameEngineEvent<GameEngineCtfEvent>>,
    pub oddball_events: Vec<GameEngineEvent<GameEngineOddballEvent>>,
    pub king_events: Vec<GameEngineEvent<GameEngineKingEvent>>,
    pub vip_events: Vec<GameEngineEvent<GameEngineVipEvent>>,
    pub juggernaut_events: Vec<GameEngineEvent<GameEngineJuggernautEvent>>,
    pub territories_events: Vec<GameEngineEvent<GameEngineTerritoriesEvent>>,
    pub assault_events: Vec<GameEngineEvent<GameEngineAssaultEvent>>,
    pub infection_events: Vec<GameEngineEvent<GameEngineInfectionEvent>>,
    pub gun_game_events: Vec<GameEngineEvent<GameEngineGunGameEvent>>,
    pub maximum_frag_count: i32,
    pub maximum_plasma_count: i32,
    pub multiplayer_constants: Vec<MultiplayerConstants>,
    pub state_responses: Vec<GameEngineStatusResponse>,
    pub hill_shader: String,
    pub pregame_intro_message: String,
    pub ctf_intro_message: String,
    pub slayerintro_message: String,
    pub oddball_intro_message: String,
    pub king_intro_message: String,
    pub sandbox_intro_message: String,
    pub vip_intro_message: String,
    pub juggernaut_intro_message: String,
    pub territories_intro_message: String,
    pub assault_intro_message: String,
    pub infection_intro_message: String,
    pub survival_intro_message: String,
    pub gun_game_intro_message: String,
    pub campaign_intro_message: String,
}

impl RuntimeGlobals {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            editor_biped: s.read_tag_ref_path("editor biped").unwrap_or_default(),
            editor_helper: s.read_tag_ref_path("editor helper").unwrap_or_default(),
            flag: s.read_tag_ref_path("flag").unwrap_or_default(),
            ball: s.read_tag_ref_path("ball").unwrap_or_default(),
            assault_bomb: s.read_tag_ref_path("assault bomb").unwrap_or_default(),
            vip_influence_area: s.read_tag_ref_path("vip influence area").unwrap_or_default(),
            in_game_text: s.read_tag_ref_path("in game text").unwrap_or_default(),
            sounds: read_block_vec(s, "sounds", Sound::from_struct),
            looping_sounds: read_block_vec(s, "looping sounds", LoopingSound::from_struct),
            general_events: read_block_vec(s, "general events", GameEngineEvent::<GameEngineGeneralEvent>::from_struct),
            flavor_events: read_block_vec(s, "flavor events", GameEngineEvent::<GameEngineFlavorEvent>::from_struct),
            slayer_events: read_block_vec(s, "slayer events", GameEngineEvent::<GameEngineSlayerEvent>::from_struct),
            ctf_events: read_block_vec(s, "ctf events", GameEngineEvent::<GameEngineCtfEvent>::from_struct),
            oddball_events: read_block_vec(s, "oddball events", GameEngineEvent::<GameEngineOddballEvent>::from_struct),
            king_events: read_block_vec(s, "king events", GameEngineEvent::<GameEngineKingEvent>::from_struct),
            vip_events: read_block_vec(s, "vip events", GameEngineEvent::<GameEngineVipEvent>::from_struct),
            juggernaut_events: read_block_vec(s, "juggernaut events", GameEngineEvent::<GameEngineJuggernautEvent>::from_struct),
            territories_events: read_block_vec(s, "territories events", GameEngineEvent::<GameEngineTerritoriesEvent>::from_struct),
            assault_events: read_block_vec(s, "assault events", GameEngineEvent::<GameEngineAssaultEvent>::from_struct),
            infection_events: read_block_vec(s, "infection events", GameEngineEvent::<GameEngineInfectionEvent>::from_struct),
            gun_game_events: read_block_vec(s, "gun game events", GameEngineEvent::<GameEngineGunGameEvent>::from_struct),
            maximum_frag_count: s.read_int_any("maximum frag count").unwrap_or_default() as i32,
            maximum_plasma_count: s.read_int_any("maximum plasma count").unwrap_or_default() as i32,
            multiplayer_constants: read_block_vec(s, "multiplayer constants", MultiplayerConstants::from_struct),
            state_responses: read_block_vec(s, "state responses", GameEngineStatusResponse::from_struct),
            hill_shader: s.read_tag_ref_path("hill shader").unwrap_or_default(),
            pregame_intro_message: s.read_tag_ref_path("pregame intro message").unwrap_or_default(),
            ctf_intro_message: s.read_tag_ref_path("ctf intro message").unwrap_or_default(),
            slayerintro_message: s.read_tag_ref_path("slayerintro message").unwrap_or_default(),
            oddball_intro_message: s.read_tag_ref_path("oddball intro message").unwrap_or_default(),
            king_intro_message: s.read_tag_ref_path("king intro message").unwrap_or_default(),
            sandbox_intro_message: s.read_tag_ref_path("sandbox intro message").unwrap_or_default(),
            vip_intro_message: s.read_tag_ref_path("vip intro message").unwrap_or_default(),
            juggernaut_intro_message: s.read_tag_ref_path("juggernaut intro message").unwrap_or_default(),
            territories_intro_message: s.read_tag_ref_path("territories intro message").unwrap_or_default(),
            assault_intro_message: s.read_tag_ref_path("assault intro message").unwrap_or_default(),
            infection_intro_message: s.read_tag_ref_path("infection intro message").unwrap_or_default(),
            survival_intro_message: s.read_tag_ref_path("survival intro message").unwrap_or_default(),
            gun_game_intro_message: s.read_tag_ref_path("gun game intro message").unwrap_or_default(),
            campaign_intro_message: s.read_tag_ref_path("campaign intro message").unwrap_or_default(),
        }
    }
}
//================================================================================
// Root: MultiplayerGlobals + from_tag
//================================================================================

/// Errors from `multiplayer_globals` tag walking.
#[derive(Debug)]
pub enum MultiplayerGlobalsError {
    /// The tag's group FOURCC was not `mulg`.
    WrongGroup { actual: [u8; 4] },
}

impl std::fmt::Display for MultiplayerGlobalsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { actual } => write!(
                f,
                "tag group '{}' is not 'mulg' — not a multiplayer_globals tag",
                std::str::from_utf8(actual).unwrap_or("?"),
            ),
        }
    }
}

impl std::error::Error for MultiplayerGlobalsError {}

/// Root `multiplayer_globals_struct_definition`. Holds the two top-level
/// blocks: the shared `universal` globals and the runtime game-engine data.
#[derive(Debug, Clone, Default)]
pub struct MultiplayerGlobals {
    pub universal: Vec<UniversalGlobals>,
    pub runtime: Vec<RuntimeGlobals>,
}

impl MultiplayerGlobals {
    /// Walk a parsed `multiplayer_globals` (mulg) tag into the
    /// schema-faithful type tree.
    pub fn from_tag(tag: &TagFile) -> Result<Self, MultiplayerGlobalsError> {
        let actual = tag.group().tag.to_be_bytes();
        if &actual != b"mulg" {
            return Err(MultiplayerGlobalsError::WrongGroup { actual });
        }
        let root = tag.root();
        Ok(Self {
            universal: read_block_vec(&root, "universal", UniversalGlobals::from_struct),
            runtime: read_block_vec(&root, "runtime", RuntimeGlobals::from_struct),
        })
    }
}

/// Helper: walk a tag block field and collect parsed elements.
fn read_block_vec<T, F>(s: &TagStruct<'_>, name: &str, mut f: F) -> Vec<T>
where
    F: FnMut(&TagStruct<'_>) -> T,
{
    s.field(name)
        .and_then(|f| f.as_block())
        .map(|block| block.iter().map(|e| f(&e)).collect::<Vec<_>>())
        .unwrap_or_default()
}

//================================================================================
// Tests
//================================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use strum::VariantArray;

    #[test]
    fn enum_defaults() {
        assert_eq!(GameEngineGeneralEvent::default(), GameEngineGeneralEvent::Kill);
        assert_eq!(GameEngineStatus::default(), GameEngineStatus::WaitingForSpaceToClear);
        assert_eq!(GameEngineEventAudience::default(), GameEngineEventAudience::CausePlayer);
        assert_eq!(GameEngineEventInput::default(), GameEngineEventInput::NONE);
    }

    #[test]
    fn strum_roundtrips() {
        // IntoStaticStr yields the exact schema option string.
        let s: &'static str = GameEngineGeneralEvent::DeathFromTheGrave.into();
        assert_eq!(s, "death from the grave");
        let s: &'static str = GameEngineFlavorEvent::Steaktacular.into();
        assert_eq!(s, "steaktacular");
        // EnumString parses the schema option string (case-insensitive) back.
        assert_eq!(
            "flag captured".parse::<GameEngineCtfEvent>().unwrap(),
            GameEngineCtfEvent::FlagCaptured
        );
        assert_eq!(
            "SPLATTER 10X".parse::<GameEngineFlavorEvent>().unwrap(),
            GameEngineFlavorEvent::Splatter10x
        );
        let s: &'static str = GameEngineSoundResponseFlags::AnnouncerSound.into();
        assert_eq!(s, "announcer sound");
    }

    #[test]
    fn variant_counts_match_schema() {
        assert_eq!(GameEngineGeneralEvent::VARIANTS.len(), 59);
        assert_eq!(GameEngineFlavorEvent::VARIANTS.len(), 110);
        assert_eq!(GameEngineStatus::VARIANTS.len(), 29);
        assert_eq!(GameEngineAssaultEvent::VARIANTS.len(), 18);
    }
}
