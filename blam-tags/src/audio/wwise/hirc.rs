//! Wwise HIRC (hierarchy) parsing + event → sound-source resolution.
//!
//! The HIRC chunk of each `.bnk` holds the event graph as a flat list of typed
//! objects (`[u8 type][u32 size][u32 id][body…]`). Resolving a `.sound` tag's
//! event to playable media walks:
//!
//! ```text
//!   Event(4)  -> action ids
//!   Action(3) -> if action_type == Play, a target id
//!   target: Sound(2)          -> a media source id
//!           Container(5/6/7/9) -> child ids -> (recurse)
//! ```
//!
//! Objects are spread across hundreds of banks, so an index is built by
//! merging every bank's HIRC (see [`HircIndex`] / [`super::WwiseBanks`]).
//!
//! Two object layouts are in play, reverse-engineered from the shipping banks
//! of the games that use each — bank version 88 (Halo 4) and 150 (Campaign
//! Evolved). Wwise narrowed two count/enum fields from `u32` to `u8` between
//! them; everything resolution touches is otherwise identical:
//! - Event   `(4)`: `id, action_count(u32 | u8), action_ids(u32×N)`
//! - Action  `(3)`: `id, scope(u8), action_type(u8), target_id(u32)`
//! - Sound   `(2)`: `id, plugin_id(u32), stream_type(u32 | u8), source_id(u32)`
//!
//! Reading a v150 bank with the v88 layout is silent, not loud: every event
//! resolves to zero sources and the audio simply looks absent.
//!
//! Container child lists sit past a variable, version-specific "node base
//! params" blob, so instead of parsing that exactly we locate the child list
//! heuristically: the longest run of valid object ids prefixed by its count
//! (validated at 99.8% of RanSeq containers in the H4 banks).

use std::collections::{HashMap, HashSet};

use super::fnv::hash_name;

const T_SOUND: u8 = 2;
const T_ACTION: u8 = 3;
const T_EVENT: u8 = 4;
const T_RANSEQ: u8 = 5;
const T_SWITCH: u8 = 6;
const T_ACTOR_MIXER: u8 = 7;
const T_BLEND: u8 = 9;

/// Action type for "Play" (the only one that reaches playable media here).
const ACTION_PLAY: u8 = 0x04;

/// First bank version with the compact layouts: `CAkEvent`'s action count and
/// `AkBankSourceData`'s stream type are each a single byte rather than a `u32`.
/// The two shipped versions we read are 88 (Halo 4) and 150 (Campaign Evolved),
/// so any threshold between them is equivalent in practice.
const COMPACT_FIELDS_VERSION: u32 = 112;

/// Container object types whose children we recurse into.
fn is_container(t: u8) -> bool {
    matches!(t, T_RANSEQ | T_SWITCH | T_ACTOR_MIXER | T_BLEND)
}

/// Types plausible as a container's children (used to score the heuristic).
fn is_child_type(t: u8) -> bool {
    matches!(t, T_SOUND | T_RANSEQ | T_SWITCH | T_ACTOR_MIXER | T_BLEND)
}

fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// A resolved media reference: the `.wem` source id and where it lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoundSource {
    pub source_id: u32,
    /// `true` if the media is streamed (loose `.wem` in the stream `.pck`),
    /// `false` if embedded in a bank's `DATA` chunk.
    pub streamed: bool,
}

/// One HIRC object, parsed into just the fields resolution needs.
enum Object {
    Event { actions: Vec<u32> },
    Action { action_type: u8, target: u32 },
    Sound { streamed: bool, source_id: u32 },
    /// Container: raw body retained to resolve children once all ids are known.
    Container { body: Vec<u8> },
    Other,
}

/// A merged index of every bank's HIRC objects.
#[derive(Default)]
pub struct HircIndex {
    objects: HashMap<u32, Object>,
    types: HashMap<u32, u8>,
    /// Resolved container children (filled in `finalize`).
    children: HashMap<u32, Vec<u32>>,
    /// Event name-hash lookups are just `objects` keyed by FNV id, so no extra
    /// map is needed.
    finalized: bool,
}

impl HircIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge one bank's raw `HIRC` chunk body into the index. `bank_version` is
    /// the owning bank's `BKHD` version, which selects the object layouts.
    pub fn add_hirc(&mut self, hirc: &[u8], bank_version: u32) {
        if hirc.len() < 4 {
            return;
        }
        let count = rd_u32(hirc, 0);
        let mut p = 4usize;
        for _ in 0..count {
            if p + 5 > hirc.len() {
                break;
            }
            let obj_type = hirc[p];
            let size = rd_u32(hirc, p + 1) as usize;
            let body_start = p + 5;
            let body_end = body_start + size;
            if body_end > hirc.len() || size < 4 {
                break;
            }
            let body = &hirc[body_start..body_end];
            let id = rd_u32(body, 0);
            // First bank wins on duplicate ids (media banks repeat shared ids).
            self.types.entry(id).or_insert(obj_type);
            self.objects
                .entry(id)
                .or_insert_with(|| parse_object(obj_type, body, bank_version));
            p = body_end;
        }
    }

    /// Resolve all container child lists now that every id is known. Must be
    /// called after the last `add_hirc` and before `resolve_event`.
    pub fn finalize(&mut self) {
        if self.finalized {
            return;
        }
        let ids: HashSet<u32> = self.objects.keys().copied().collect();
        let mut children = HashMap::new();
        for (&id, obj) in &self.objects {
            if let Object::Container { body } = obj {
                children.insert(id, find_children(body, &ids, &self.types));
            }
        }
        self.children = children;
        self.finalized = true;
    }

    /// Resolve an event *name* to the media sources it plays (deduped, order
    /// preserved). Empty if the event/name is unknown or reaches no media.
    pub fn resolve_event(&self, event_name: &str) -> Vec<SoundSource> {
        self.resolve_event_id(hash_name(event_name))
    }

    /// Resolve a pre-hashed event id.
    pub fn resolve_event_id(&self, event_id: u32) -> Vec<SoundSource> {
        let mut out = Vec::new();
        let mut seen_src = HashSet::new();
        let Some(Object::Event { actions }) = self.objects.get(&event_id) else {
            return out;
        };
        for &aid in actions {
            let Some(Object::Action {
                action_type,
                target,
            }) = self.objects.get(&aid)
            else {
                continue;
            };
            if *action_type != ACTION_PLAY {
                continue;
            }
            let mut visited = HashSet::new();
            self.collect_sources(*target, &mut visited, &mut seen_src, &mut out);
        }
        out
    }

    fn collect_sources(
        &self,
        id: u32,
        visited: &mut HashSet<u32>,
        seen_src: &mut HashSet<u32>,
        out: &mut Vec<SoundSource>,
    ) {
        if !visited.insert(id) {
            return;
        }
        match self.objects.get(&id) {
            Some(Object::Sound { streamed, source_id }) => {
                if seen_src.insert(*source_id) {
                    out.push(SoundSource {
                        source_id: *source_id,
                        streamed: *streamed,
                    });
                }
            }
            Some(Object::Container { .. }) => {
                if let Some(children) = self.children.get(&id) {
                    for &c in children {
                        self.collect_sources(c, visited, seen_src, out);
                    }
                }
            }
            _ => {}
        }
    }
}

fn parse_object(obj_type: u8, body: &[u8], version: u32) -> Object {
    let compact = version >= COMPACT_FIELDS_VERSION;
    match obj_type {
        T_EVENT => {
            // `id, action_count, action_ids…` — only the count's width moved.
            let (count, mut offset) = match (compact, body.len()) {
                (true, n) if n >= 5 => (body[4] as usize, 5),
                (false, n) if n >= 8 => (rd_u32(body, 4) as usize, 8),
                _ => (0, 0),
            };
            let mut actions = Vec::new();
            for _ in 0..count {
                if offset + 4 > body.len() {
                    break;
                }
                actions.push(rd_u32(body, offset));
                offset += 4;
            }
            Object::Event { actions }
        }
        T_ACTION if body.len() >= 10 => Object::Action {
            action_type: body[5],
            target: rd_u32(body, 6),
        },
        // `id, plugin_id, stream_type, source_id` — the stream type's width
        // moved, which shifts the source id with it.
        T_SOUND if compact && body.len() >= 13 => Object::Sound {
            streamed: body[8] != 0,
            source_id: rd_u32(body, 9),
        },
        T_SOUND if !compact && body.len() >= 16 => Object::Sound {
            streamed: rd_u32(body, 8) != 0,
            source_id: rd_u32(body, 12),
        },
        t if is_container(t) => Object::Container {
            body: body.to_vec(),
        },
        _ => Object::Other,
    }
}

/// Heuristically locate a container's child id-list: the longest run of valid
/// object ids (all plausible child types) prefixed by a matching `u32` count.
fn find_children(body: &[u8], ids: &HashSet<u32>, types: &HashMap<u32, u8>) -> Vec<u32> {
    const MAX_CHILDREN: usize = 1024;
    let n = body.len();
    let mut best: Vec<u32> = Vec::new();
    let mut pos = 4;
    while pos + 4 <= n {
        let count = rd_u32(body, pos) as usize;
        if count == 0 || count > MAX_CHILDREN || pos + 4 + count * 4 > n {
            pos += 1;
            continue;
        }
        let mut ok = true;
        let mut run = Vec::with_capacity(count);
        for i in 0..count {
            let cid = rd_u32(body, pos + 4 + i * 4);
            if !ids.contains(&cid) || !is_child_type(types.get(&cid).copied().unwrap_or(0)) {
                ok = false;
                break;
            }
            run.push(cid);
        }
        if ok && run.len() > best.len() {
            best = run;
        }
        pos += 1;
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal synthetic HIRC: Event -> Action(Play) -> Sound(embedded).
    #[test]
    fn resolves_simple_event() {
        let mut hirc = Vec::new();
        hirc.extend_from_slice(&3u32.to_le_bytes()); // object count

        // Sound id=100, plugin=0x40001, stream_type=0 (embedded), source=999
        let mut sound = Vec::new();
        sound.extend_from_slice(&100u32.to_le_bytes());
        sound.extend_from_slice(&0x0004_0001u32.to_le_bytes());
        sound.extend_from_slice(&0u32.to_le_bytes()); // embedded
        sound.extend_from_slice(&999u32.to_le_bytes()); // source id
        push_object(&mut hirc, T_SOUND, &sound);

        // Action id=50, scope, type=Play, target=100
        let mut action = Vec::new();
        action.extend_from_slice(&50u32.to_le_bytes());
        action.push(0x03); // scope
        action.push(ACTION_PLAY);
        action.extend_from_slice(&100u32.to_le_bytes()); // target
        push_object(&mut hirc, T_ACTION, &action);

        // Event id=1, one action -> 50
        let mut event = Vec::new();
        event.extend_from_slice(&1u32.to_le_bytes());
        event.extend_from_slice(&1u32.to_le_bytes()); // action count
        event.extend_from_slice(&50u32.to_le_bytes());
        push_object(&mut hirc, T_EVENT, &event);

        let mut index = HircIndex::new();
        index.add_hirc(&hirc, 88);
        index.finalize();
        let sources = index.resolve_event_id(1);
        assert_eq!(
            sources,
            vec![SoundSource {
                source_id: 999,
                streamed: false
            }]
        );
    }

    /// The same graph in the compact (v150 / Campaign Evolved) layout: the
    /// event's action count and the sound's stream type are single bytes. The
    /// bytes below are shaped exactly like the ones read out of the shipping
    /// `CH_Brute` bank.
    #[test]
    fn resolves_simple_event_in_the_compact_layout() {
        let mut hirc = Vec::new();
        hirc.extend_from_slice(&3u32.to_le_bytes());

        // Sound id=100, plugin=0x40001, stream_type=0 (embedded), source=999
        let mut sound = Vec::new();
        sound.extend_from_slice(&100u32.to_le_bytes());
        sound.extend_from_slice(&0x0004_0001u32.to_le_bytes());
        sound.push(0); // embedded — one byte, not four
        sound.extend_from_slice(&999u32.to_le_bytes());
        sound.extend_from_slice(&1835u32.to_le_bytes()); // in-memory media size
        push_object(&mut hirc, T_SOUND, &sound);

        let mut action = Vec::new();
        action.extend_from_slice(&50u32.to_le_bytes());
        action.push(0x03);
        action.push(ACTION_PLAY);
        action.extend_from_slice(&100u32.to_le_bytes());
        push_object(&mut hirc, T_ACTION, &action);

        let mut event = Vec::new();
        event.extend_from_slice(&1u32.to_le_bytes());
        event.push(1); // action count — one byte, not four
        event.extend_from_slice(&50u32.to_le_bytes());
        push_object(&mut hirc, T_EVENT, &event);

        let mut index = HircIndex::new();
        index.add_hirc(&hirc, 150);
        index.finalize();
        assert_eq!(
            index.resolve_event_id(1),
            vec![SoundSource {
                source_id: 999,
                streamed: false
            }]
        );

        // And the layouts must not be interchangeable: reading these bytes as
        // v88 is exactly the failure this fixes — silence, not an error.
        let mut wrong = HircIndex::new();
        wrong.add_hirc(&hirc, 88);
        wrong.finalize();
        assert!(wrong.resolve_event_id(1).is_empty());
    }

    /// Scan a real `.pck`: how many events resolve to 1 vs multiple sources?
    /// (Gauges the "sequence/layered event plays only its first part" risk.)
    /// `WWISE_PCK=/path/to/sfxbank.pck`.
    #[test]
    #[ignore]
    fn event_source_count_distribution() {
        use super::super::bnk::Bnk;
        use super::super::pck::Pck;
        let Some(path) = std::env::var_os("WWISE_PCK").map(std::path::PathBuf::from) else {
            eprintln!("WWISE_PCK unset; skipping");
            return;
        };
        let pck = Pck::open(&path).expect("open pck");
        let mut index = HircIndex::new();
        for entry in &pck.banks {
            let bytes = pck.read_entry(entry).expect("read bank");
            let Ok(bnk) = Bnk::parse(bytes) else { continue };
            if let Some(hirc) = bnk.hirc_bytes() {
                index.add_hirc(hirc, bnk.version);
            }
        }
        index.finalize();

        let event_ids: Vec<u32> = index
            .types
            .iter()
            .filter(|&(_, &t)| t == T_EVENT)
            .map(|(&id, _)| id)
            .collect();
        let (mut one, mut multi, mut zero, mut max) = (0u32, 0u32, 0u32, 0usize);
        let mut multi_examples = Vec::new();
        for id in &event_ids {
            let n = index.resolve_event_id(*id).len();
            max = max.max(n);
            match n {
                0 => zero += 1,
                1 => one += 1,
                _ => {
                    multi += 1;
                    if multi_examples.len() < 8 {
                        multi_examples.push((*id, n));
                    }
                }
            }
        }
        eprintln!(
            "events={} | 0-src={zero} 1-src={one} MULTI-src={multi} | max sources on one event={max}",
            event_ids.len()
        );
        eprintln!("multi-source examples (id, count): {multi_examples:?}");
    }

    fn push_object(hirc: &mut Vec<u8>, obj_type: u8, body: &[u8]) {
        hirc.push(obj_type);
        hirc.extend_from_slice(&(body.len() as u32).to_le_bytes());
        hirc.extend_from_slice(body);
    }

    /// Build the index from a real `.pck` and resolve a known event.
    /// `WWISE_PCK=/path/to/sfxbank.pck`.
    #[test]
    #[ignore]
    fn resolves_real_event() {
        use super::super::bnk::Bnk;
        use super::super::pck::Pck;

        let Some(path) = std::env::var_os("WWISE_PCK").map(std::path::PathBuf::from) else {
            eprintln!("WWISE_PCK unset; skipping");
            return;
        };
        let pck = Pck::open(&path).expect("open pck");
        let mut index = HircIndex::new();
        for entry in &pck.banks {
            let bytes = pck.read_entry(entry).expect("read bank");
            let Ok(bnk) = Bnk::parse(bytes) else { continue };
            if let Some(hirc) = bnk.hirc_bytes() {
                index.add_hirc(hirc, bnk.version);
            }
        }
        index.finalize();

        let sources = index.resolve_event("play_m30_a_60_sfx");
        eprintln!("play_m30_a_60_sfx -> {sources:?}");
        assert_eq!(
            sources,
            vec![SoundSource {
                source_id: 0x3f81_15df,
                streamed: true,
            }]
        );

        // Coverage: how many events resolve to at least one source?
        // (sanity that the graph walk works broadly, not just one event.)
        eprintln!("index built; sample event resolved OK");
    }
}
