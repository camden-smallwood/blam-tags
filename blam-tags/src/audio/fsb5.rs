//! FSB5 (FMOD Sound Bank v5) container parser.
//!
//! Halo 3's audio ships in `<game>/fmod/pc/sfx.fsb` — a single FSB5 bank
//! holding every cinematic dialogue line, music stem and foley sound as
//! an FMOD-Vorbis subsound. Layout (all little-endian on PC):
//!
//! ```text
//!   [ 60-byte FSB5_HEADER ]
//!   [ headerChunkSizeBytes : packed 64-bit sample headers + metadata ]
//!   [ namesChunkSizeBytes  : u32 offset table + NUL-terminated names ]
//!   [ dataChunkSizeBytes   : concatenated per-subsound sample data ]
//! ```
//!
//! Each sample header is a packed `u64` (FMOD field layout, see the
//! shift/mask constants below) followed by an optional chain of 32-bit
//! metadata chunks. For Vorbis the `VORBISCODECSETUPDATA` chunk carries
//! the CRC32 that selects the shared codebook setup (see
//! [`super::vorbis`]).
//!
//! The bank is ~700 MB, so we parse only the header + name regions
//! (~1 MB) up front and read each subsound's compressed bytes lazily.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

// --- Packed sample-header bitfields (FMOD fmod_codec_fsb5.h) ---------------
const SUBSOUND_LENGTH_SHIFT: u64 = 34; // numSamples (30 bits)
const SUBSOUND_OFFSET_SHIFT: u64 = 7; // data offset / 32 (27 bits)
const SUBSOUND_CHANNELS_SHIFT: u64 = 5; // channel enum (2 bits)
const SUBSOUND_FREQUENCY_SHIFT: u64 = 1; // frequency enum (4 bits)
const SUBSOUND_LENGTH_MASK: u64 = 0x3FFF_FFFF;
const SUBSOUND_OFFSET_MASK: u64 = 0x07FF_FFFF;
const SUBSOUND_CHANNELS_MASK: u64 = 0x3;
const SUBSOUND_FREQUENCY_MASK: u64 = 0xF;
const SUBSOUND_METADATA_MASK: u64 = 0x1;

// --- Packed metadata-chunk bitfields ---------------------------------------
const METADATA_ID_SHIFT: u32 = 25; // chunk type (7 bits)
const METADATA_LENGTH_SHIFT: u32 = 1; // chunk length (24 bits)
const METADATA_ID_MASK: u32 = 0x7F;
const METADATA_LENGTH_MASK: u32 = 0x00FF_FFFF;
const METADATA_NEXT_MASK: u32 = 0x1;

// --- Metadata chunk types we care about (FSB_METADATA enum) ----------------
const META_CHANNELS: u32 = 1; // FSB_METADATA_CHANNELS: u8 channel count
const META_FREQUENCY: u32 = 2;
const META_VORBIS_SETUP: u32 = 11; // FSB_METADATA_VORBISCODECSETUPDATA
// Halo 3's banks store non-standard channel counts (e.g. 4ch music, which
// the 2-bit packed enum can't express) in this u32 chunk. Verified against
// vgmstream: subsounds carrying this chunk decode with exactly its value.
const META_CHANNEL_COUNT_U32: u32 = 14;

const FSB5_HEADER_SIZE: u64 = 60;
const FMOD_SOUND_FORMAT_VORBIS: u32 = 15;

/// One subsound (a single decodable stream) within the bank.
#[derive(Debug, Clone)]
pub struct SubSound {
    /// Original source name from the bank's name table (may be empty).
    pub name: String,
    pub channels: u32,
    pub frequency: u32,
    /// Absolute byte offset of this subsound's compressed data in the file.
    pub data_offset: u64,
    /// Length in bytes of this subsound's compressed data.
    pub data_len: u64,
    /// Total decoded PCM sample frames (per channel).
    pub num_samples: u32,
    /// CRC32 selecting the shared Vorbis codebook setup, if Vorbis.
    pub setup_hash: u32,
}

/// Parsed FSB5 bank directory (headers + names). Sample data stays on disk.
#[derive(Debug)]
pub struct Fsb5 {
    path: PathBuf,
    /// Compression format applied to all subsounds (15 = Vorbis).
    pub data_format: u32,
    pub subsounds: Vec<SubSound>,
    /// Lowercased subsound name → index (from the bank name table).
    name_index: HashMap<String, usize>,
    /// Lowercased `data\sound\...\*.aif` path → index (from `.fsb.info`).
    path_index: HashMap<String, usize>,
    /// `fmod bank subsound id hash` → index (from `.fsb.info`, offset 0 of each
    /// record). This is the engine's authoritative, collision-free permutation
    /// key — see [`fmod_bank_subsound_id_hash`].
    id_index: HashMap<u32, usize>,
}

const FSB_INFO_RECORD_BYTES: usize = 280;

fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn rd_u64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes([
        b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5], b[o + 6], b[o + 7],
    ])
}

fn channel_enum_to_count(e: u64) -> u32 {
    match e {
        0 => 1,
        1 => 2,
        2 => 6,
        3 => 8,
        _ => 1,
    }
}
fn frequency_enum_to_hz(e: u64) -> u32 {
    match e {
        0 => 4000,
        1 => 8000,
        2 => 11000,
        3 => 12000,
        4 => 16000,
        5 => 22050,
        6 => 24000,
        7 => 32000,
        8 => 44100,
        9 => 48000,
        10 => 96000,
        _ => 48000,
    }
}

impl Fsb5 {
    /// Parse the bank's directory (header + sample headers + names). Reads
    /// ~1 MB regardless of total bank size; subsound data is read lazily by
    /// [`Fsb5::read_subsound_data`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;

        let mut hdr = [0u8; FSB5_HEADER_SIZE as usize];
        file.read_exact(&mut hdr)
            .map_err(|e| format!("read header: {e}"))?;
        if &hdr[0..4] != b"FSB5" {
            return Err(format!(
                "not an FSB5 bank (magic {:02x?})",
                &hdr[0..4]
            ));
        }
        let num_subsounds = rd_u32(&hdr, 8) as usize;
        let header_chunk = rd_u32(&hdr, 12) as u64;
        let names_chunk = rd_u32(&hdr, 16) as u64;
        let data_format = rd_u32(&hdr, 24);

        let data_start = FSB5_HEADER_SIZE + header_chunk + names_chunk;

        // Read the packed sample-header region and the name region together.
        let mut sample_headers = vec![0u8; header_chunk as usize];
        file.read_exact(&mut sample_headers)
            .map_err(|e| format!("read sample headers: {e}"))?;
        let mut names = vec![0u8; names_chunk as usize];
        if names_chunk > 0 {
            file.read_exact(&mut names)
                .map_err(|e| format!("read names: {e}"))?;
        }

        // Pass 1: decode every sample header (offset/channels/freq/metadata).
        // Offsets in the header are relative (×32 from data_start); we keep
        // them relative for now and resolve the per-subsound length by the
        // delta to the next subsound's offset.
        struct Raw {
            rel_offset: u64,
            channels: u32,
            frequency: u32,
            num_samples: u32,
            setup_hash: u32,
        }
        let mut raws: Vec<Raw> = Vec::with_capacity(num_subsounds);
        let mut pos = 0usize;
        for _ in 0..num_subsounds {
            if pos + 8 > sample_headers.len() {
                return Err("sample header region truncated".into());
            }
            let t = rd_u64(&sample_headers, pos);
            pos += 8;

            let rel_offset = ((t >> SUBSOUND_OFFSET_SHIFT) & SUBSOUND_OFFSET_MASK) * 32;
            let mut channels =
                channel_enum_to_count((t >> SUBSOUND_CHANNELS_SHIFT) & SUBSOUND_CHANNELS_MASK);
            let mut frequency =
                frequency_enum_to_hz((t >> SUBSOUND_FREQUENCY_SHIFT) & SUBSOUND_FREQUENCY_MASK);
            let num_samples = ((t >> SUBSOUND_LENGTH_SHIFT) & SUBSOUND_LENGTH_MASK) as u32;
            let has_metadata = (t & SUBSOUND_METADATA_MASK) != 0;

            let mut setup_hash = 0u32;
            if has_metadata {
                // Walk the 32-bit chunk-header chain.
                loop {
                    if pos + 4 > sample_headers.len() {
                        return Err("metadata chunk header truncated".into());
                    }
                    let ch = rd_u32(&sample_headers, pos);
                    pos += 4;
                    let ctype = (ch >> METADATA_ID_SHIFT) & METADATA_ID_MASK;
                    let clen = ((ch >> METADATA_LENGTH_SHIFT) & METADATA_LENGTH_MASK) as usize;
                    let has_next = (ch & METADATA_NEXT_MASK) != 0;
                    if pos + clen > sample_headers.len() {
                        return Err("metadata chunk body truncated".into());
                    }
                    let body = &sample_headers[pos..pos + clen];
                    match ctype {
                        META_CHANNELS if !body.is_empty() => channels = body[0] as u32,
                        META_CHANNEL_COUNT_U32 if body.len() >= 4 => channels = rd_u32(body, 0),
                        META_FREQUENCY if body.len() >= 4 => frequency = rd_u32(body, 0),
                        META_VORBIS_SETUP if body.len() >= 4 => setup_hash = rd_u32(body, 0),
                        _ => {}
                    }
                    pos += clen;
                    if !has_next {
                        break;
                    }
                }
            }

            raws.push(Raw {
                rel_offset,
                channels,
                frequency,
                num_samples,
                setup_hash,
            });
        }

        // Pass 2: resolve per-subsound data length from neighbouring offsets.
        let total_data = {
            let f_len = file
                .seek(SeekFrom::End(0))
                .map_err(|e| format!("seek end: {e}"))?;
            f_len.saturating_sub(data_start)
        };
        let mut subsounds = Vec::with_capacity(num_subsounds);
        for i in 0..raws.len() {
            let start = raws[i].rel_offset;
            let end = if i + 1 < raws.len() {
                raws[i + 1].rel_offset
            } else {
                total_data
            };
            subsounds.push(SubSound {
                name: String::new(),
                channels: raws[i].channels,
                frequency: raws[i].frequency,
                data_offset: data_start + start,
                data_len: end.saturating_sub(start),
                num_samples: raws[i].num_samples,
                setup_hash: raws[i].setup_hash,
            });
        }

        // Names: a u32 offset table (one per subsound) into the name region,
        // each pointing at a NUL-terminated string.
        let mut name_index = HashMap::new();
        if names_chunk as usize >= num_subsounds * 4 {
            for (i, ss) in subsounds.iter_mut().enumerate() {
                let off = rd_u32(&names, i * 4) as usize;
                if off < names.len() {
                    let end = names[off..]
                        .iter()
                        .position(|&b| b == 0)
                        .map(|p| off + p)
                        .unwrap_or(names.len());
                    let name = String::from_utf8_lossy(&names[off..end]).into_owned();
                    name_index.insert(name.to_ascii_lowercase(), i);
                    ss.name = name;
                }
            }
        }

        let (path_index, id_index) =
            load_info_indexes(path.with_extension("fsb.info"), num_subsounds);

        Ok(Self {
            path,
            data_format,
            subsounds,
            name_index,
            path_index,
            id_index,
        })
    }

    /// Map a tag-relative sound path to a subsound index via the `.fsb.info`
    /// manifest (`sound\foo\bar` → `data\sound\foo\bar.aif`).
    pub fn index_for_tag_path(&self, sound_rel: &str) -> Option<usize> {
        let key = tag_ref_to_info_path(sound_rel);
        self.path_index.get(&key).copied()
    }

    /// True if subsounds are FMOD-Vorbis (the only format we decode).
    pub fn is_vorbis(&self) -> bool {
        self.data_format == FMOD_SOUND_FORMAT_VORBIS
    }

    /// Exact subsound-name lookup (case-insensitive).
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.name_index.get(&name.to_ascii_lowercase()).copied()
    }

    /// Look up a subsound by its `fmod bank subsound id hash` — the engine's own
    /// permutation key (see [`fmod_bank_subsound_id_hash`]). Unlike the name/path
    /// lookups this is collision-free: the hash folds in the full tag path,
    /// pitch range, and permutation string-id, so it uniquely identifies the
    /// intended variant even when hundreds of tags share a leaf name.
    pub fn index_of_id(&self, id: u32) -> Option<usize> {
        self.id_index.get(&id).copied()
    }

    /// Resolve a tag path or leaf name to a subsound index.
    pub fn resolve_sound(&self, sound_rel: &str) -> Option<usize> {
        if let Some(i) = self.index_for_tag_path(sound_rel) {
            return Some(i);
        }
        let leaf = sound_leaf_name(sound_rel);
        self.index_of(&leaf).or_else(|| self.find_substr(&leaf))
    }

    /// First subsound whose name contains `needle` (case-insensitive).
    pub fn find_substr(&self, needle: &str) -> Option<usize> {
        let n = needle.to_ascii_lowercase();
        self.subsounds
            .iter()
            .position(|s| s.name.to_ascii_lowercase().contains(&n))
    }

    /// Read a subsound's raw compressed bytes (the length-prefixed Vorbis
    /// packet stream) from disk.
    pub fn read_subsound_data(&self, index: usize) -> Result<Vec<u8>, String> {
        let ss = self
            .subsounds
            .get(index)
            .ok_or_else(|| format!("subsound index {index} out of range"))?;
        let mut file = File::open(&self.path).map_err(|e| format!("reopen bank: {e}"))?;
        file.seek(SeekFrom::Start(ss.data_offset))
            .map_err(|e| format!("seek data: {e}"))?;
        let mut buf = vec![0u8; ss.data_len as usize];
        file.read_exact(&mut buf)
            .map_err(|e| format!("read data: {e}"))?;
        Ok(buf)
    }
}

/// Normalize a tag reference to the `.fsb.info` manifest key.
pub fn tag_ref_to_info_path(sound_rel: &str) -> String {
    let norm = sound_rel.replace('/', "\\");
    let mut p = if norm.to_ascii_lowercase().starts_with("data\\") {
        norm
    } else {
        format!("data\\{norm}")
    };
    if !p.to_ascii_lowercase().ends_with(".aif") {
        p.push_str(".aif");
    }
    p.to_ascii_lowercase()
}

pub fn sound_leaf_name(rel: &str) -> String {
    rel.rsplit(['\\', '/'])
        .next()
        .unwrap_or(rel)
        .to_string()
}

/// Parse the `<bank>.fsb.info` sidecar into both lookup maps. Each 280-byte
/// record is `record[i] ↔ subsound[i]`: a 24-byte header whose first `u32` is
/// the `fmod bank subsound id hash`, followed by the source path
/// (`data\sound\...\<perm>.aif`). Returns `(path→index, id→index)`.
fn load_info_indexes(
    info_path: PathBuf,
    num_subsounds: usize,
) -> (HashMap<String, usize>, HashMap<u32, usize>) {
    let Ok(info) = std::fs::read(&info_path) else {
        return (HashMap::new(), HashMap::new());
    };
    let mut paths = HashMap::new();
    let mut ids = HashMap::new();
    let count = info.len() / FSB_INFO_RECORD_BYTES;
    for i in 0..count.min(num_subsounds) {
        let rec = &info[i * FSB_INFO_RECORD_BYTES..(i + 1) * FSB_INFO_RECORD_BYTES];
        // The subsound id hash is the record's leading u32.
        ids.insert(rd_u32(rec, 0), i);
        if let Some(start) = rec.windows(5).position(|w| w == b"data\\") {
            let end = rec[start..]
                .iter()
                .position(|&b| b == 0)
                .map(|p| start + p)
                .unwrap_or(rec.len());
            let path = String::from_utf8_lossy(&rec[start..end]).to_ascii_lowercase();
            paths.insert(path, i);
        }
    }
    (paths, ids)
}

/// Compute the `fmod bank subsound id hash` the Halo 3+/MCC engine uses to
/// resolve a sound permutation to an FMOD-bank subsound — the collision-free key
/// stored per subsound in `<bank>.fsb.info` (see [`Fsb5::index_of_id`]).
///
/// Reverse-engineered from `reach_tag_test.exe` (`sub_140256B00`): the engine
/// assembles the source path `data\<tag_path>[\<pitch_range>]\<permutation>`
/// (backslashes, no extension) and runs **djb2** (seed 5381, multiplier 33) over
/// it starting at byte 11 — skipping the constant `data\sound\` prefix (only when
/// the assembled string is longer than 11 bytes). We lowercase the inputs so the
/// result matches the manifest's canonical (lowercase) ids regardless of how the
/// caller cased the path.
///
/// - `tag_path`: the `.sound` tag path without extension, e.g.
///   `sound\dialog\combat\civilian_1\default\01_contact\ambush`.
/// - `pitch_range`: the pitch-range folder to fold in, or `None` for the default
///   single range (decide with [`fmod_pitch_range_folder`]).
/// - `permutation`: the permutation's raw `name` string-id from the tag.
pub fn fmod_bank_subsound_id_hash(
    tag_path: &str,
    pitch_range: Option<&str>,
    permutation: &str,
) -> u32 {
    fn push_norm(out: &mut String, part: &str) {
        for ch in part.chars() {
            out.push(if ch == '/' { '\\' } else { ch.to_ascii_lowercase() });
        }
    }
    let mut s = String::with_capacity(tag_path.len() + permutation.len() + 24);
    s.push_str("data\\");
    push_norm(&mut s, tag_path);
    s.push('\\');
    if let Some(pr) = pitch_range {
        push_norm(&mut s, pr);
        s.push('\\');
    }
    push_norm(&mut s, permutation);

    // djb2 over the path minus the constant `data\sound\` prefix (11 bytes).
    let bytes = s.as_bytes();
    let tail = if bytes.len() > 11 { &bytes[11..] } else { bytes };
    let mut hash: u32 = 5381;
    for &b in tail {
        hash = (b as u32).wrapping_add(hash.wrapping_mul(33));
    }
    hash
}

/// Whether the pitch-range name should be folded into the FMOD hash/path,
/// matching the engine: included when the tag has more than one pitch range, or
/// when the single range's name is not the implicit `|default|`.
pub fn fmod_pitch_range_folder(pitch_range: &str, multiple_ranges: bool) -> Option<&str> {
    if multiple_ranges || !pitch_range.eq_ignore_ascii_case("|default|") {
        Some(pitch_range)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::fmod_bank_subsound_id_hash as h;

    // Vectors reverse-engineered from reach_tag_test.exe (sub_140256B00) and
    // verified against haloreach_mcc/fmod/pc/*.fsb.info id columns.
    #[test]
    fn subsound_id_hash_matches_engine() {
        // Dialogue: civilian_1 ambush permutations (single |default| pitch range).
        let ambush = r"sound\dialog\combat\civilian_1\default\01_contact\ambush";
        assert_eq!(h(ambush, None, "ambush_1"), 0x0a77_3133);
        assert_eq!(h(ambush, None, "ambush_2"), 0x0a77_3134);
        assert_eq!(h(ambush, None, "ambush_5"), 0x0a77_3137);

        // Length sensitivity: charge1 vs charge10 hash to unrelated regions.
        let charge = r"sound\dialog\combat\grunt2\default\02_combat\charge";
        assert_eq!(h(charge, None, "charge1"), 0x3b31_4bb6);
        assert_eq!(h(charge, None, "charge10"), 0xa15a_c2a6);

        // SFX: the permutation string-id (not the .aif filename) is what's hashed.
        let dash = r"sound\characters\footsteps\chief\concrete_h3\dash";
        assert_eq!(h(dash, None, "concrete_dash9"), 0x7d1d_e8af);

        // Case/separator normalization must not change the result.
        assert_eq!(
            h("SOUND/dialog/combat/civilian_1/default/01_contact/ambush", None, "AMBUSH_1"),
            0x0a77_3133
        );
    }
}
