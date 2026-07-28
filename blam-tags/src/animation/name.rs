//! Animation-name grammar + base-state resolution.
//!
//! Halo animation names are colon-delimited token strings encoding a
//! `(mode, weapon_class, weapon_type, set, state)` scope plus optional
//! damage / transition / variant suffixes — e.g. `combat:rifle:idle`,
//! `combat:any:aim_move_down`, `combat:any:s_ping:back:gut`,
//! `combat:rifle:idle:var1`. This module ports Foundry's
//! `utils.AnimationName` tokenizer ([`AnimationName`]) and its
//! `_base_state_candidates` heuristic ([`base_state_candidates`]).
//!
//! These drive overlay/replacement composition: an overlay stores
//! deltas authored relative to a *base* pose (the matching locomotion
//! or idle stance), not the bind pose. To reconstruct the in-engine
//! pose we must find that base animation and compose onto its first
//! frame — see [`super::Animation::overlay_base_pose`]. Both TagTool
//! (`GetBaseAnimation`) and Foundry (`_get_base_animation_candidates`)
//! do this; we mirror Foundry's (more complete) base-state priority.

/// `damage_states` from Foundry `managed_blam/animation.py`.
const DAMAGE_STATES: &[&str] = &["h_ping", "s_ping", "h_kill", "s_kill"];
/// `directions`.
const DIRECTIONS: &[&str] = &["front", "left", "right", "back"];
/// `regions`.
const REGIONS: &[&str] = &[
    "gut", "chest", "head", "l_arm", "l_hand", "l_leg", "l_foot", "r_arm", "r_hand", "r_leg",
    "r_foot",
];

/// What kind of state an animation name encodes. Mirrors Foundry's
/// `AnimationStateType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationStateType {
    /// Normal action/overlay (idle, aim, reload, …).
    Action,
    /// Damage reaction (`*_ping` / `*_kill` + direction + region).
    Damage,
    /// State-to-state transition (contains a `2` separator token).
    Transition,
}

/// Parsed animation name. Faithful port of Foundry's
/// `utils.AnimationName`. Unparsed components default to `"any"`
/// (matching Halo's wildcard convention); `valid` is `false` for empty
/// or single-token (`custom`) names.
#[derive(Debug, Clone)]
pub struct AnimationName {
    pub mode: String,
    pub weapon_class: String,
    pub weapon_type: String,
    pub set: String,
    pub state: String,
    pub variant: String,
    pub state_type: AnimationStateType,
    /// Single-token name with no scope — not eligible as a composition
    /// base source.
    pub custom: bool,
    pub valid: bool,
}

impl AnimationName {
    /// Tokenize + parse an animation name, mirroring Foundry's
    /// `AnimationName.__init__` token-popping grammar.
    pub fn parse(name: &str) -> Self {
        let mut out = Self {
            mode: "any".into(),
            weapon_class: "any".into(),
            weapon_type: "any".into(),
            set: "any".into(),
            state: "any".into(),
            variant: String::new(),
            state_type: AnimationStateType::Action,
            custom: false,
            valid: false,
        };

        // tokenise(): replace ':' with space, lowercase, split on
        // whitespace.
        let mut tokens: Vec<String> = name
            .to_ascii_lowercase()
            .split(|c: char| c == ':' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .map(|t| t.to_string())
            .collect();

        if tokens.is_empty() {
            return out;
        }

        // Trailing `var*` is a variant suffix.
        if tokens.last().is_some_and(|t| t.starts_with("var")) {
            out.variant = tokens.pop().unwrap();
        }

        // Single remaining token → custom (and not `valid`).
        if tokens.len() == 1 {
            out.custom = true;
            return out;
        }

        // Damage: `… <damage_state> <direction> <region>`.
        if tokens.len() > 2 && REGIONS.contains(&tokens[tokens.len() - 1].as_str()) {
            let dir = &tokens[tokens.len() - 2];
            let dmg = &tokens[tokens.len() - 3];
            if DIRECTIONS.contains(&dir.as_str()) && DAMAGE_STATES.contains(&dmg.as_str()) {
                out.state_type = AnimationStateType::Damage;
                tokens.pop(); // region
                tokens.pop(); // direction
            }
        } else if tokens.len() > 2 && tokens.iter().any(|t| t == "2") {
            // Transition: a `2` separator with tokens on both sides.
            let index_2 = tokens.iter().position(|t| t == "2").unwrap();
            if index_2 > 0 && index_2 < tokens.len() - 1 {
                out.state_type = AnimationStateType::Transition;
                tokens.pop(); // destination_state
                if tokens.last().map(|s| s.as_str()) == Some("2") {
                    tokens.pop();
                } else {
                    while tokens.last().map(|s| s.as_str()) != Some("2") {
                        tokens.pop();
                    }
                    tokens.pop();
                }
            }
        }

        // State is the last remaining token; scope is popped from the
        // front (mode, weapon_class, weapon_type, set).
        out.state = tokens.pop().unwrap_or_else(|| "any".into());
        if !tokens.is_empty() {
            out.mode = tokens.remove(0);
        }
        if !tokens.is_empty() {
            out.weapon_class = tokens.remove(0);
        }
        if !tokens.is_empty() {
            out.weapon_type = tokens.remove(0);
        }
        if !tokens.is_empty() {
            out.set = tokens.remove(0);
        }

        out.valid = true;
        out
    }
}

/// Ordered, de-duplicated list of base-animation *states* to try when
/// resolving the composition base for an overlay/replacement, in
/// priority order. Port of Foundry's `_base_state_candidates` for the
/// overlay/replacement branch.
///
/// The intent: an `aim_move_down` overlay composes onto the
/// `move_down` locomotion base (with `_fast`/`_slow` family members),
/// degrading to `move`, and finally `idle`. A bare aim/look overlay
/// composes onto `idle`. This matches the in-engine layering far
/// better than always using `idle` (TagTool's cruder choice).
pub fn base_state_candidates(state: &str) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();

    let add = |c: &mut Vec<String>, s: &str| {
        if !s.is_empty() && !c.iter().any(|x| x == s) {
            c.push(s.to_string());
        }
    };
    // `add_family`: a state plus its slow/fast siblings (or the
    // de-suffixed root if it already is a `_fast`/`_slow`).
    let add_family = |c: &mut Vec<String>, s: &str| {
        let s = s.trim_matches('_');
        if s.is_empty() {
            return;
        }
        add(c, s);
        if let Some(root) = s.strip_suffix("_fast").or_else(|| s.strip_suffix("_slow")) {
            add(c, root);
        } else {
            add(c, &format!("{s}_fast"));
            add(c, &format!("{s}_slow"));
        }
    };

    if state.starts_with("aim_airborne") || state.starts_with("look_airborne") {
        add(&mut candidates, "airborne");
        return candidates;
    }

    let tokens: Vec<&str> = state.split('_').collect();
    const MOTION: &[&str] = &["move", "walk", "run", "jog", "locomote", "turn"];
    const DIR: &[&str] = &["front", "right", "left", "back"];

    for index in 0..tokens.len() {
        if !DIR.contains(&tokens[index]) {
            continue;
        }
        // Scan backwards for a motion verb starting the run.
        for start in (0..index).rev() {
            if !MOTION.contains(&tokens[start]) {
                continue;
            }
            add_family(&mut candidates, &tokens[start..=index].join("_"));
            if tokens[start] == "locomote" && start + 1 < index {
                add_family(&mut candidates, &tokens[start + 1..=index].join("_"));
            }
        }
    }

    for prefix in ["aim_", "look_", "acc_", "steer_"] {
        if let Some(stripped) = state.strip_prefix(prefix) {
            add_family(&mut candidates, stripped);
            for suffix in ["_up", "_down", "_left", "_right"] {
                if let Some(root) = stripped.strip_suffix(suffix) {
                    add_family(&mut candidates, root);
                }
            }
        }
    }

    add(&mut candidates, "idle");
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mode_class_state() {
        let n = AnimationName::parse("combat:rifle:idle");
        assert!(n.valid && !n.custom);
        assert_eq!((n.mode.as_str(), n.weapon_class.as_str(), n.state.as_str()), ("combat", "rifle", "idle"));
        assert_eq!(n.state_type, AnimationStateType::Action);
    }

    #[test]
    fn parses_any_weapon_class() {
        let n = AnimationName::parse("combat:any:aim_move_down");
        assert_eq!((n.mode.as_str(), n.weapon_class.as_str(), n.state.as_str()), ("combat", "any", "aim_move_down"));
    }

    #[test]
    fn strips_variant() {
        let n = AnimationName::parse("combat:rifle:idle:var1");
        assert_eq!(n.state, "idle");
        assert_eq!(n.variant, "var1");
    }

    #[test]
    fn parses_damage() {
        let n = AnimationName::parse("combat:any:s_ping:back:gut");
        assert_eq!(n.state_type, AnimationStateType::Damage);
        // region/direction popped; state is the damage_state token.
        assert_eq!(n.state, "s_ping");
        assert_eq!((n.mode.as_str(), n.weapon_class.as_str()), ("combat", "any"));
    }

    #[test]
    fn single_token_is_custom() {
        let n = AnimationName::parse("death");
        assert!(n.custom && !n.valid);
    }

    #[test]
    fn base_states_aim_move_down() {
        // aim_move_down → move_down family → move family → idle.
        let c = base_state_candidates("aim_move_down");
        assert_eq!(c.first().map(String::as_str), Some("move_down"));
        assert!(c.contains(&"move".to_string()));
        assert_eq!(c.last().map(String::as_str), Some("idle"));
    }

    #[test]
    fn base_states_bare_aim_is_idle() {
        let c = base_state_candidates("aim_still");
        assert_eq!(c.last().map(String::as_str), Some("idle"));
    }

    #[test]
    fn base_states_airborne_shortcuts() {
        let c = base_state_candidates("aim_airborne");
        assert_eq!(c, vec!["airborne".to_string()]);
    }
}
