//! Wwise SoundBank (`.bnk`) chunk parser.
//!
//! A `.bnk` is a flat sequence of `[4-byte tag][u32 size][body]` chunks. The
//! ones we care about for Halo 4 (bank version 88):
//!
//! ```text
//!   BKHD  bank header: version(u32), soundbank id(u32), ...
//!   DIDX  data index: N × (id(u32), offset(u32), size(u32)) into DATA
//!   DATA  concatenated embedded .wem media (indexed by DIDX)
//!   HIRC  the event graph (Events, Actions, Sounds, containers) — parsed
//!         separately in `hirc.rs`
//!   STID/STMG/ENVS/... metadata we skip
//! ```
//!
//! Media-only banks carry `BKHD`+`DIDX`+`DATA` and no `HIRC`; the event graph
//! lives in other banks. Streamed sounds have no DIDX entry — their bytes are
//! loose `.wem` in `sfxstream.pck` instead. Resolution therefore spans every
//! bank (see the parent module's `WwiseBanks`).

fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// One embedded-media entry from a `DIDX` chunk: `size` bytes at `offset`
/// within the bank's `DATA` chunk body.
#[derive(Debug, Clone, Copy)]
pub struct DidxEntry {
    pub id: u32,
    pub offset: u32,
    pub size: u32,
}

/// A parsed SoundBank. Owns the bank's bytes so embedded media and the raw
/// HIRC region can be sliced out without re-reading.
#[derive(Debug)]
pub struct Bnk {
    /// Bank format version from `BKHD` (88 for Halo 4).
    pub version: u32,
    /// Soundbank id from `BKHD` (matches the `.pck` bank entry id).
    pub soundbank_id: u32,
    /// Embedded-media index (empty for HIRC-only / streamed banks).
    pub didx: Vec<DidxEntry>,
    bytes: Vec<u8>,
    data_range: Option<(usize, usize)>,
    hirc_range: Option<(usize, usize)>,
}

impl Bnk {
    /// Parse a bank blob into its chunk directory. The blob is retained so
    /// embedded media / HIRC bytes can be borrowed from it.
    pub fn parse(bytes: Vec<u8>) -> Result<Self, String> {
        let mut version = 0;
        let mut soundbank_id = 0;
        let mut didx = Vec::new();
        let mut data_range = None;
        let mut hirc_range = None;

        let mut p = 0usize;
        while p + 8 <= bytes.len() {
            let tag = &bytes[p..p + 4];
            let size = rd_u32(&bytes, p + 4) as usize;
            let body = p + 8;
            let end = body.checked_add(size).filter(|&e| e <= bytes.len());
            let Some(end) = end else {
                return Err(format!(
                    "bnk chunk {:?} at {p} overruns ({size} bytes)",
                    String::from_utf8_lossy(tag)
                ));
            };
            match tag {
                b"BKHD" if size >= 8 => {
                    version = rd_u32(&bytes, body);
                    soundbank_id = rd_u32(&bytes, body + 4);
                }
                b"DIDX" => {
                    for e in bytes[body..end].chunks_exact(12) {
                        didx.push(DidxEntry {
                            id: rd_u32(e, 0),
                            offset: rd_u32(e, 4),
                            size: rd_u32(e, 8),
                        });
                    }
                }
                b"DATA" => data_range = Some((body, end)),
                b"HIRC" => hirc_range = Some((body, end)),
                _ => {}
            }
            p = end;
        }

        if version == 0 {
            return Err("bnk has no BKHD chunk".into());
        }
        Ok(Self {
            version,
            soundbank_id,
            didx,
            bytes,
            data_range,
            hirc_range,
        })
    }

    /// True if this bank carries an event graph.
    pub fn has_hirc(&self) -> bool {
        self.hirc_range.is_some()
    }

    /// The raw `HIRC` chunk body, if present (parsed by `hirc.rs`).
    pub fn hirc_bytes(&self) -> Option<&[u8]> {
        self.hirc_range.map(|(a, b)| &self.bytes[a..b])
    }

    /// Borrow an embedded `.wem` by its source id (via the DIDX → DATA map).
    pub fn embedded_wem(&self, id: u32) -> Option<&[u8]> {
        let (data_start, data_end) = self.data_range?;
        let e = self.didx.iter().find(|e| e.id == id)?;
        let a = data_start + e.offset as usize;
        let b = a + e.size as usize;
        (b <= data_end).then(|| &self.bytes[a..b])
    }

    /// Embedded media as `(source_id, offset_within_bank, size)` — lets the
    /// index record each `.wem`'s absolute `.pck` location for lazy reads
    /// without holding bank bytes.
    pub fn embedded_locations(&self) -> impl Iterator<Item = (u32, usize, usize)> + '_ {
        let data = self.data_range;
        self.didx.iter().filter_map(move |e| {
            let (start, _end) = data?;
            Some((e.id, start + e.offset as usize, e.size as usize))
        })
    }

    /// All embedded `.wem` as `(id, bytes)` — used by the sweep/index build.
    pub fn embedded_wems(&self) -> impl Iterator<Item = (u32, &[u8])> {
        let data = self.data_range;
        self.didx.iter().filter_map(move |e| {
            let (data_start, data_end) = data?;
            let a = data_start + e.offset as usize;
            let b = a + e.size as usize;
            (b <= data_end).then(|| (e.id, &self.bytes[a..b]))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::pck::Pck;
    use super::*;
    use std::path::PathBuf;

    fn pck_path() -> Option<PathBuf> {
        std::env::var_os("WWISE_PCK").map(PathBuf::from)
    }

    #[test]
    fn parses_bank_and_extracts_wem() {
        let Some(path) = pck_path() else {
            eprintln!("WWISE_PCK unset; skipping");
            return;
        };
        let pck = Pck::open(&path).expect("open pck");

        let mut media_banks = 0;
        let mut hirc_banks = 0;
        let mut total_wems = 0;
        let mut checked_riff = false;

        for entry in &pck.banks {
            let bytes = pck.read_entry(entry).expect("read bank");
            let bnk = Bnk::parse(bytes).expect("parse bank");
            assert_eq!(bnk.version, 88, "expected Halo 4 bank version");
            if !bnk.didx.is_empty() {
                media_banks += 1;
                total_wems += bnk.didx.len();
                // Every embedded entry should be a RIFF/WAVE .wem.
                if !checked_riff {
                    let (id, wem) = bnk.embedded_wems().next().unwrap();
                    assert_eq!(&wem[0..4], b"RIFF", "wem {id} should be RIFF");
                    assert_eq!(&wem[8..12], b"WAVE", "wem {id} should be WAVE");
                    checked_riff = true;
                }
            }
            if bnk.has_hirc() {
                hirc_banks += 1;
            }
        }
        eprintln!(
            "{} banks: {media_banks} with embedded media ({total_wems} wem), {hirc_banks} with HIRC",
            pck.banks.len()
        );
        assert!(total_wems > 0 && checked_riff);
    }
}
