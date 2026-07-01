//! Wwise name hashing (FNV-1, 32-bit, lowercased).
//!
//! Wwise identifies events/objects by a 32-bit hash of their (lowercased) name.
//! A Halo 4 `.sound` tag stores the event *name* (`play_m30_a_60_sfx`); hashing
//! it yields the HIRC Event id to resolve against (see [`super::hirc`]).

/// FNV-1 32-bit hash of `name`, lowercased (ASCII) — the Wwise short-id scheme.
pub fn hash_name(name: &str) -> u32 {
    const OFFSET_BASIS: u32 = 2166136261;
    const PRIME: u32 = 16777619;
    let mut h = OFFSET_BASIS;
    for b in name.bytes() {
        let b = b.to_ascii_lowercase();
        h = h.wrapping_mul(PRIME);
        h ^= b as u32;
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_event_hashes() {
        // Verified against the Halo 4 sfxbank HIRC.
        assert_eq!(hash_name("play_m30_a_60_sfx"), 848788103);
        // Case-insensitive.
        assert_eq!(hash_name("PLAY_M30_A_60_SFX"), 848788103);
    }
}
