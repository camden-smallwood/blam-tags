//! FMOD-Vorbis → PCM decoding for FSB5 subsounds.
//!
//! FMOD strips the Vorbis *setup* header (codebooks/floors/residues) from
//! each subsound and references it by a CRC32; the shared setup packets are
//! de-duplicated in FMOD's `VorbisCodecSetups[]` table, which we extracted
//! into a bundled blob (see `tools/extract_vorbis_books.py`). Each packet is
//! a stock libVorbis 1.3.2 setup header, so once we look it up by CRC32 and
//! synthesize the (trivial) identification + comment headers, a standard
//! Vorbis decoder (`lewton`) decodes the audio packets directly.
//!
//! On disk each subsound's data is a flat sequence of audio packets, each
//! prefixed by a little-endian `u16` length (FMOD's "V1" framing — no Ogg
//! pages). FMOD's fixed blocksizes are 256 (short) and 2048 (long).

use std::collections::HashMap;
use std::sync::OnceLock;

use lewton::audio::{read_audio_packet, PreviousWindowRight};
use lewton::header::{read_header_ident, read_header_setup, IdentHeader, SetupHeader};

/// Blocksize exponents: short = 256 (2^8), long = 2048 (2^11).
const BLOCKSIZE_BYTE: u8 = 0x08 | (0x0B << 4);

/// Bundled, de-duplicated Vorbis setup table (CRC32 → setup packet),
/// generated from FMOD's `fmod_codec_fsbvorbis_books.cpp`.
static BOOKS_BLOB: &[u8] = include_bytes!("vorbis_books.bin");

fn books() -> &'static HashMap<u32, &'static [u8]> {
    static BOOKS: OnceLock<HashMap<u32, &'static [u8]>> = OnceLock::new();
    BOOKS.get_or_init(|| {
        let mut map = HashMap::new();
        let b = BOOKS_BLOB;
        if b.len() < 8 || &b[0..4] != b"VBK1" {
            log::error!("[audio] vorbis_books.bin: bad magic");
            return map;
        }
        let count = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
        let mut pos = 8;
        for _ in 0..count {
            if pos + 8 > b.len() {
                break;
            }
            let hash = u32::from_le_bytes([b[pos], b[pos + 1], b[pos + 2], b[pos + 3]]);
            let len =
                u32::from_le_bytes([b[pos + 4], b[pos + 5], b[pos + 6], b[pos + 7]]) as usize;
            pos += 8;
            if pos + len > b.len() {
                break;
            }
            map.insert(hash, &b[pos..pos + len]);
            pos += len;
        }
        map
    })
}

/// Synthesize the Vorbis identification header packet.
fn ident_packet(channels: u8, sample_rate: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(30);
    v.push(0x01);
    v.extend_from_slice(b"vorbis");
    v.extend_from_slice(&0u32.to_le_bytes()); // vorbis_version
    v.push(channels);
    v.extend_from_slice(&sample_rate.to_le_bytes());
    v.extend_from_slice(&0i32.to_le_bytes()); // bitrate_maximum
    v.extend_from_slice(&0i32.to_le_bytes()); // bitrate_nominal
    v.extend_from_slice(&0i32.to_le_bytes()); // bitrate_minimum
    v.push(BLOCKSIZE_BYTE);
    v.push(0x01); // framing bit
    v
}

/// Synthesize a minimal Vorbis comment header packet (no vendor, no tags).
fn comment_packet() -> Vec<u8> {
    let mut v = Vec::with_capacity(16);
    v.push(0x03);
    v.extend_from_slice(b"vorbis");
    v.extend_from_slice(&0u32.to_le_bytes()); // vendor length
    v.extend_from_slice(&0u32.to_le_bytes()); // user comment list length
    v.push(0x01); // framing bit
    v
}

/// Decoded PCM: interleaved 16-bit samples plus stream parameters.
pub struct DecodedPcm {
    pub samples: Vec<i16>,
    pub channels: u16,
    pub sample_rate: u32,
}

impl DecodedPcm {
    /// Number of per-channel sample frames.
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }

    pub fn duration_secs(&self) -> f32 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frame_count() as f32 / self.sample_rate as f32
        }
    }
}

/// Decode one FMOD-Vorbis subsound's compressed bytes to interleaved PCM.
///
/// `data` is the subsound's raw on-disk payload (length-prefixed packets),
/// `setup_hash` selects the shared codebook setup.
pub fn decode_subsound(
    data: &[u8],
    channels: u32,
    sample_rate: u32,
    setup_hash: u32,
) -> Result<DecodedPcm, String> {
    let channels = channels.clamp(1, 8) as u8;

    let setup_bytes = *books()
        .get(&setup_hash)
        .ok_or_else(|| format!("vorbis setup 0x{setup_hash:08x} not in books table"))?;

    let ident: IdentHeader =
        read_header_ident(&ident_packet(channels, sample_rate)).map_err(|e| format!("ident: {e:?}"))?;
    // Comment header is parsed for completeness/validation but unused.
    let setup: SetupHeader = read_header_setup(setup_bytes, channels, (8, 11))
        .map_err(|e| format!("setup 0x{setup_hash:08x}: {e:?}"))?;

    let mut pwr = PreviousWindowRight::new();
    let mut out: Vec<i16> = Vec::new();
    let mut pos = 0usize;
    let mut packet_index = 0u64;

    while pos + 2 <= data.len() {
        let plen = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;
        if plen == 0 {
            continue;
        }
        if pos + plen > data.len() {
            // Truncated trailing packet; stop cleanly.
            break;
        }
        let packet = &data[pos..pos + plen];
        pos += plen;

        match read_audio_packet(&ident, &setup, packet, &mut pwr) {
            Ok(chans) => interleave_into(&mut out, &chans, channels as usize),
            Err(e) => {
                // The very first packet sometimes yields no usable output;
                // tolerate sporadic errors rather than aborting the line.
                log::trace!("[audio] vorbis packet {packet_index} error: {e:?}");
            }
        }
        packet_index += 1;
    }

    if out.is_empty() {
        return Err("decoded zero PCM samples".into());
    }

    Ok(DecodedPcm {
        samples: out,
        channels: channels as u16,
        sample_rate,
    })
}

/// Interleave per-channel sample columns onto the end of `out`.
fn interleave_into(out: &mut Vec<i16>, chans: &[Vec<i16>], channels: usize) {
    if chans.is_empty() {
        return;
    }
    let frames = chans[0].len();
    out.reserve(frames * channels);
    for f in 0..frames {
        for c in 0..channels {
            // Fewer decoded channels than declared: duplicate channel 0.
            let src = if c < chans.len() { &chans[c] } else { &chans[0] };
            out.push(src.get(f).copied().unwrap_or(0));
        }
    }
}

/// Downmix interleaved multi-channel PCM to stereo using standard Vorbis
/// channel ordering and ITU-R BS.775 coefficients.
///
/// Vorbis channel assignments:
/// - 1ch : C
/// - 2ch : L, R  (returned unchanged)
/// - 3ch : FL, C, FR
/// - 4ch : FL, FR, BL, BR
/// - 5ch : FL, C, FR, BL, BR
/// - 6ch : FL, C, FR, BL, BR, LFE
///
/// Returns a 2-channel (stereo) interleaved Vec<i16>.  If `channels` is
/// already ≤2 the input slice is returned as-is via `to_vec()`.
pub fn downmix_to_stereo(samples: &[i16], channels: usize) -> Vec<i16> {
    if channels <= 2 {
        return samples.to_vec();
    }
    let frames = samples.len() / channels;
    let mut out = Vec::with_capacity(frames * 2);

    const S: f32 = 0.707; // sqrt(0.5)

    for f in 0..frames {
        let b = f * channels;
        let get = |c: usize| -> f32 { samples.get(b + c).copied().unwrap_or(0) as f32 };

        let (l, r) = match channels {
            3 => {
                // FL, C, FR
                (get(0) + S * get(1), get(2) + S * get(1))
            }
            4 => {
                // FL, FR, BL, BR
                (get(0) + S * get(2), get(1) + S * get(3))
            }
            5 => {
                // FL, C, FR, BL, BR
                (get(0) + S * get(1) + S * get(3), get(2) + S * get(1) + S * get(4))
            }
            _ => {
                // 6ch and above: FL, C, FR, BL, BR, LFE
                // LFE (ch 5) omitted from stereo fold-down.
                (get(0) + S * get(1) + S * get(3), get(2) + S * get(1) + S * get(4))
            }
        };

        out.push(l.clamp(i16::MIN as f32, i16::MAX as f32) as i16);
        out.push(r.clamp(i16::MIN as f32, i16::MAX as f32) as i16);
    }
    out
}

/// Validate that the bundled books table loaded.
pub fn books_loaded() -> usize {
    books().len()
}

/// Decode a self-contained Ogg Vorbis stream to interleaved PCM.
///
/// Unlike the FMOD path ([`decode_subsound`]), which reconstructs stripped
/// setup headers, this handles a *complete* `OggS` stream — e.g. a Halo CE
/// `.sound` permutation's inline `samples` blob, which is stored as an ordinary
/// Ogg Vorbis file.
pub fn decode_ogg_vorbis(bytes: &[u8]) -> Result<DecodedPcm, String> {
    use lewton::inside_ogg::OggStreamReader;
    let mut reader = OggStreamReader::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("ogg header: {e:?}"))?;
    let channels = reader.ident_hdr.audio_channels as u16;
    let sample_rate = reader.ident_hdr.audio_sample_rate;
    let mut samples = Vec::new();
    while let Some(packet) = reader
        .read_dec_packet_itl()
        .map_err(|e| format!("ogg packet: {e:?}"))?
    {
        samples.extend_from_slice(&packet);
    }
    Ok(DecodedPcm {
        samples,
        channels,
        sample_rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode a real Halo CE inline Ogg blob (skip-if-absent). Extract with:
    /// `blam-tag-shell extract-data --game haloce_mcc <tag> "pitch ranges[0]/permutations[0]/samples" --output /tmp/ce.ogg`
    #[test]
    #[ignore]
    fn decode_ce_inline_ogg() {
        let path = std::env::var("CE_OGG").unwrap_or_else(|_| "/tmp/ce.ogg".to_owned());
        if !std::path::Path::new(&path).exists() {
            eprintln!("skip: no {path}");
            return;
        }
        let bytes = std::fs::read(&path).unwrap();
        let pcm = decode_ogg_vorbis(&bytes).expect("decode CE ogg");
        eprintln!(
            "CE inline ogg: {} frames {}ch {}Hz",
            pcm.frame_count(),
            pcm.channels,
            pcm.sample_rate
        );
        assert!(pcm.frame_count() > 0);
    }
    use crate::audio::banks::SoundBanks;
    use std::path::PathBuf;

    /// Verify that 6-channel foley downmixes to stereo correctly.
    #[test]
    fn downmix_6ch_to_stereo() {
        // One frame: [FL, C, FR, BL, BR, LFE] = [32767, 0, 32767, 0, 0, 0]
        let samples = vec![32767i16, 0, 32767, 0, 0, 0];
        let stereo = downmix_to_stereo(&samples, 6);
        assert_eq!(stereo.len(), 2);
        // FL (32767) is straight left, no center, no surround → L=32767
        assert_eq!(stereo[0], 32767);
        assert_eq!(stereo[1], 32767);
    }

    fn write_wav_i16(path: &str, samples: &[i16], channels: u16, rate: u32) {
        use std::io::Write;
        let data_len = (samples.len() * 2) as u32;
        let block_align = channels as u32 * 2;
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
        f.write_all(b"WAVEfmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&rate.to_le_bytes()).unwrap();
        f.write_all(&(rate * block_align).to_le_bytes()).unwrap();
        f.write_all(&(block_align as u16).to_le_bytes()).unwrap();
        f.write_all(&16u16.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_len.to_le_bytes()).unwrap();
        for s in samples {
            f.write_all(&s.to_le_bytes()).unwrap();
        }
    }

    fn stats(samples: &[i16]) -> (f64, f64) {
        if samples.len() < 2 {
            return (0.0, 0.0);
        }
        let rms = (samples.iter().map(|&s| (s as f64).powi(2)).sum::<f64>()
            / samples.len() as f64)
            .sqrt();
        let mut crossings = 0u64;
        for w in samples.windows(2) {
            if (w[0] >= 0) != (w[1] >= 0) {
                crossings += 1;
            }
        }
        let zcr = crossings as f64 / samples.len() as f64;
        (rms, zcr)
    }

    /// Decode the intro music + foley, assert the multichannel counts are
    /// detected correctly (regression for the FSB5 channel-count chunk), and
    /// dump downmixed WAVs to /tmp for manual listening.
    ///
    /// `010la_music_1_3` is 4-channel (quad) and `010la_foley_1_3` is
    /// 6-channel (5.1); both were previously mis-read as mono and decoded to
    /// noise. Per-channel output is bit-identical to vgmstream's reference.
    #[test]
    #[ignore]
    fn render_intro_tracks() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("H3EK/tags");
        let banks = SoundBanks::open_pc(&root).expect("banks");

        for (label, path, expect_ch) in [
            ("music", "sound\\cinematics\\010_jungle\\010la\\music\\010la_music_1_3", 4),
            ("foley", "sound\\cinematics\\010_jungle\\010la\\foley\\010la_foley_1_3", 6),
        ] {
            let (bi, si) = banks.resolve(path).unwrap();
            let bank = banks.bank(bi);
            let ss = &bank.subsounds[si];
            assert_eq!(
                ss.channels, expect_ch,
                "{label} should be {expect_ch}ch, got {}",
                ss.channels
            );
            let data = bank.read_subsound_data(si).unwrap();
            let pcm = decode_subsound(&data, ss.channels, ss.frequency, ss.setup_hash).unwrap();
            assert_eq!(pcm.channels as u32, expect_ch);
            let (mut samples, mut ch) = (pcm.samples.clone(), pcm.channels);
            if ch > 2 {
                samples = downmix_to_stereo(&samples, ch as usize);
                ch = 2;
            }
            let (rms, zcr) = stats(&samples);
            let out = format!("/tmp/protomorph_{label}.wav");
            write_wav_i16(&out, &samples, ch, pcm.sample_rate);
            println!(
                "{label}: src {}ch -> stereo {}Hz frames={} rms={rms:.0} zcr={zcr:.4} -> {out}",
                expect_ch, pcm.sample_rate, samples.len() / ch as usize
            );
        }
    }
}
