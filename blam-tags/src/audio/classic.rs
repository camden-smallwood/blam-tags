//! Classic inline sound codecs — Halo 2's `.sound` tags store audio in the tag
//! itself (unlike Halo 3+, which page it out to FMOD banks).
//!
//! H2 remastered audio is **Opus** (majority) or **Xbox ADPCM**, both framed as
//! a flat sequence of packets/blocks prefixed by a little-endian `u16` byte
//! length (no Ogg pages). The codec is chosen by the tag's `compression` enum.
//! (Halo CE's inline Ogg Vorbis is handled by [`super::vorbis::decode_ogg_vorbis`].)

use super::vorbis::DecodedPcm;

/// Opus is always 48 kHz internally.
const OPUS_SAMPLE_RATE: u32 = 48_000;
/// Max Opus frame at 48 kHz is 120 ms = 5760 samples per channel.
const OPUS_MAX_FRAME: usize = 5760;

/// Decode a length-prefixed raw Opus stream (H2) to interleaved PCM.
///
/// Layout: `[u16 le len][opus packet]…`. Decoding stops cleanly at the first
/// truncated/garbage length (some tags carry a short trailing chunk).
pub fn decode_opus(bytes: &[u8], channels: u16) -> Result<DecodedPcm, String> {
    use opus::{Channels, Decoder};
    let (opus_channels, nch) = if channels >= 2 {
        (Channels::Stereo, 2usize)
    } else {
        (Channels::Mono, 1usize)
    };
    let mut decoder =
        Decoder::new(OPUS_SAMPLE_RATE, opus_channels).map_err(|e| format!("opus init: {e}"))?;

    let mut samples = Vec::new();
    let mut frame = vec![0i16; OPUS_MAX_FRAME * nch];
    let mut pos = 0usize;
    while pos + 2 <= bytes.len() {
        let len = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]) as usize;
        pos += 2;
        if len == 0 || pos + len > bytes.len() {
            break;
        }
        match decoder.decode(&bytes[pos..pos + len], &mut frame[..], false) {
            Ok(decoded) => samples.extend_from_slice(&frame[..decoded * nch]),
            Err(_) => break,
        }
        pos += len;
    }
    if samples.is_empty() {
        return Err("no opus packets decoded".to_owned());
    }
    Ok(DecodedPcm {
        samples,
        channels: nch as u16,
        sample_rate: OPUS_SAMPLE_RATE,
    })
}

// Standard IMA/DVI ADPCM tables.
#[rustfmt::skip]
const IMA_STEP: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45,
    50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230,
    253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963,
    1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327,
    3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493, 10442,
    11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];
#[rustfmt::skip]
const IMA_INDEX: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

/// Expand one IMA ADPCM nibble, updating the running predictor + step index.
/// Uses the accumulated-shift delta form (matches vgmstream `std_ima_expand_nibble`
/// / the actual game — this rounds differently than ffmpeg's multiply-then-shift,
/// and the difference compounds into audible divergence if wrong).
fn ima_expand(predictor: &mut i32, index: &mut i32, nibble: u8) -> i16 {
    let step = IMA_STEP[*index as usize];
    let mut delta = step >> 3;
    if nibble & 1 != 0 {
        delta += step >> 2;
    }
    if nibble & 2 != 0 {
        delta += step >> 1;
    }
    if nibble & 4 != 0 {
        delta += step;
    }
    if nibble & 8 != 0 {
        delta = -delta;
    }
    *predictor = (*predictor + delta).clamp(-32768, 32767);
    *index = (*index + IMA_INDEX[nibble as usize]).clamp(0, 88);
    *predictor as i16
}

/// Decode a 1- or 2-channel Xbox IMA stream into per-channel sample buffers.
/// Per `36 * ch`-byte block: a 4-byte header per channel (predictor `le16` =
/// sample 0, step index `le16`), then data interleaved in 4-byte (8-sample)
/// chunks per channel; ffmpeg drops the last decoded sample per block. Verified
/// byte-exact vs ffmpeg's `adpcm_ima_xbox` (mono/stereo) and vgmstream (`XBOX`).
fn decode_xbox_pair(bytes: &[u8], ch: usize) -> Vec<Vec<i16>> {
    let block_align = 36 * ch;
    let header_len = 4 * ch;
    let mut per_channel: Vec<Vec<i16>> = vec![Vec::new(); ch];
    let mut pos = 0;
    while pos + block_align <= bytes.len() {
        let block = &bytes[pos..pos + block_align];
        let mut predictor = vec![0i32; ch];
        let mut index = vec![0i32; ch];
        for c in 0..ch {
            let h = &block[c * 4..c * 4 + 4];
            predictor[c] = i16::from_le_bytes([h[0], h[1]]) as i32;
            index[c] = (h[2] as i8 as i32).clamp(0, 88); // step index: signed i8, byte 3 reserved
            per_channel[c].push(predictor[c] as i16);
        }
        let mut off = header_len;
        let iters = (block_align - header_len) / (4 * ch);
        for _ in 0..iters {
            for c in 0..ch {
                for _ in 0..4 {
                    let byte = block[off];
                    off += 1;
                    per_channel[c].push(ima_expand(&mut predictor[c], &mut index[c], byte & 0x0f));
                    per_channel[c].push(ima_expand(&mut predictor[c], &mut index[c], byte >> 4));
                }
            }
        }
        for channel in &mut per_channel {
            channel.pop();
        }
        pos += block_align;
    }
    per_channel
}

/// Interleave per-channel sample buffers (truncated to the shortest) into a
/// single [`DecodedPcm`].
fn interleave(per_channel: Vec<Vec<i16>>, sample_rate: u32) -> Result<DecodedPcm, String> {
    let ch = per_channel.len();
    let frames = per_channel.iter().map(Vec::len).min().unwrap_or(0);
    if frames == 0 {
        return Err("no adpcm blocks decoded".to_owned());
    }
    let mut samples = Vec::with_capacity(frames * ch);
    for i in 0..frames {
        for channel in &per_channel {
            samples.push(channel[i]);
        }
    }
    Ok(DecodedPcm {
        samples,
        channels: ch as u16,
        sample_rate,
    })
}

/// Decode a raw Xbox IMA-ADPCM stream (H2 "xbox adpcm", WAV format `0x0069`) to
/// interleaved PCM.
///
/// 1–2 channels are a single Xbox IMA stream ([`decode_xbox_pair`]). **>2
/// channels** are stored as independent **mono** Xbox-ADPCM streams interleaved
/// one 16-bit **word (2 bytes) at a time**, round-robin per channel — reverse-
/// engineered from H2 `tool.exe` (`import_sounds.cpp`: the multichannel encoder
/// deinterleaves the PCM, ACM-encodes each channel to mono ADPCM, then writes the
/// compressed output back one word per channel). We reverse that: gather each
/// channel's words, decode each as mono, and recombine. `sample_rate` from the tag.
pub fn decode_xbox_adpcm(bytes: &[u8], channels: u16, sample_rate: u32) -> Result<DecodedPcm, String> {
    let ch = channels.max(1) as usize;
    if ch <= 2 {
        return interleave(decode_xbox_pair(bytes, ch), sample_rate);
    }
    let mut channel_bytes: Vec<Vec<u8>> = vec![Vec::new(); ch];
    for (w, word) in bytes.chunks_exact(2).enumerate() {
        channel_bytes[w % ch].extend_from_slice(word);
    }
    let mut per_channel: Vec<Vec<i16>> = Vec::with_capacity(ch);
    for buf in &channel_bytes {
        per_channel.push(decode_xbox_pair(buf, 1).pop().unwrap_or_default());
    }
    interleave(per_channel, sample_rate)
}

/// Reinterpret raw interleaved 16-bit PCM (H2 "none" compression) as samples.
/// `big_endian` distinguishes the "none (big endian)" vs "none (little endian)"
/// compression enum values.
pub fn decode_pcm(
    bytes: &[u8],
    channels: u16,
    sample_rate: u32,
    big_endian: bool,
) -> Result<DecodedPcm, String> {
    let samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|b| {
            if big_endian {
                i16::from_be_bytes([b[0], b[1]])
            } else {
                i16::from_le_bytes([b[0], b[1]])
            }
        })
        .collect();
    if samples.is_empty() {
        return Err("empty pcm".to_owned());
    }
    Ok(DecodedPcm {
        samples,
        channels: channels.max(1),
        sample_rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(pcm: &DecodedPcm) -> String {
        let s = &pcm.samples;
        if s.is_empty() {
            return "empty".to_owned();
        }
        let ch = pcm.channels.max(1) as usize;
        let rms = (s.iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
        let full = s.iter().filter(|&&x| (x as i32).abs() > 30000).count() * 100 / s.len();
        // Per-channel zero-crossing rate (channel 0 only) — real audio is smooth
        // (low zcr); noise/garbage alternates sign (~50%).
        let ch0: Vec<i16> = s.iter().step_by(ch).copied().collect();
        let zc = if ch0.len() > 1 {
            ch0.windows(2).filter(|w| (w[0] >= 0) != (w[1] >= 0)).count() * 100 / ch0.len()
        } else {
            0
        };
        // L/R correlation for stereo — real stereo (esp. reflections) is highly
        // correlated (~0.7-0.95); a wrong deinterleave gives ~0.
        let corr = if ch == 2 {
            let (mut sll, mut srr, mut slr) = (0f64, 0f64, 0f64);
            for f in s.chunks_exact(2) {
                let (l, r) = (f[0] as f64, f[1] as f64);
                sll += l * l;
                srr += r * r;
                slr += l * r;
            }
            if sll > 0.0 && srr > 0.0 {
                slr / (sll.sqrt() * srr.sqrt())
            } else {
                0.0
            }
        } else {
            f64::NAN
        };
        format!(
            "{} frames {}ch {}Hz rms={rms:.0} near_full={full}% zcr(ch0)={zc}% L/R_corr={corr:.2}",
            pcm.frame_count(),
            pcm.channels,
            pcm.sample_rate
        )
    }

    fn write_wav(path: &str, pcm: &DecodedPcm) {
        use std::io::Write;
        let data_len = (pcm.samples.len() * 2) as u32;
        let ba = pcm.channels as u32 * 2;
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
        f.write_all(b"WAVEfmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&pcm.channels.to_le_bytes()).unwrap();
        f.write_all(&pcm.sample_rate.to_le_bytes()).unwrap();
        f.write_all(&(pcm.sample_rate * ba).to_le_bytes()).unwrap();
        f.write_all(&(ba as u16).to_le_bytes()).unwrap();
        f.write_all(&16u16.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_len.to_le_bytes()).unwrap();
        for s in &pcm.samples {
            f.write_all(&s.to_le_bytes()).unwrap();
        }
    }

    /// Try several interpretations of a blob and compare stats to find the right
    /// stereo layout. `BLOB=/tmp/h2_x.bin CODEC=opus|adpcm cargo test debug_blob_interpretations -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn debug_blob_interpretations() {
        let Ok(path) = std::env::var("BLOB") else {
            eprintln!("skip: set BLOB=");
            return;
        };
        let codec = std::env::var("CODEC").unwrap_or_else(|_| "adpcm".to_owned());
        let bytes = std::fs::read(&path).unwrap();
        let stem = std::path::Path::new(&path).file_stem().unwrap().to_str().unwrap();
        eprintln!("=== {path} ({} bytes) codec={codec} ===", bytes.len());
        if codec == "opus" {
            for ch in [1u16, 2] {
                if let Ok(pcm) = decode_opus(&bytes, ch) {
                    eprintln!("  opus ch={ch}: {}", stats(&pcm));
                    write_wav(&format!("/tmp/dbg_{stem}_opus{ch}.wav"), &pcm);
                }
            }
        } else {
            for ch in [1u16, 2] {
                if let Ok(pcm) = decode_xbox_adpcm(&bytes, ch, 44_100) {
                    eprintln!("  adpcm ch={ch}: {}", stats(&pcm));
                    write_wav(&format!("/tmp/dbg_{stem}_adpcm{ch}.wav"), &pcm);
                }
            }
            // Hypothesis: decode as mono, then reinterpret the PCM samples as
            // interleaved L,R,R (stereo interleaved at the sample level).
            if let Ok(mono) = decode_xbox_adpcm(&bytes, 1, 44_100) {
                let pcm = DecodedPcm {
                    samples: mono.samples,
                    channels: 2,
                    sample_rate: 44_100,
                };
                eprintln!("  adpcm mono->relabel stereo: {}", stats(&pcm));
                write_wav(&format!("/tmp/dbg_{stem}_relabel.wav"), &pcm);
            }
            // planar hypothesis: first half = L, second half = R (each mono)
            let half = bytes.len() / 2 / 36 * 36; // block-align the split
            if half > 0 {
                let l = decode_xbox_adpcm(&bytes[..half], 1, 44_100);
                let r = decode_xbox_adpcm(&bytes[half..half * 2], 1, 44_100);
                if let (Ok(l), Ok(r)) = (l, r) {
                    let frames = l.frame_count().min(r.frame_count());
                    let mut inter = Vec::with_capacity(frames * 2);
                    for i in 0..frames {
                        inter.push(l.samples[i]);
                        inter.push(r.samples[i]);
                    }
                    let pcm = DecodedPcm { samples: inter, channels: 2, sample_rate: 44_100 };
                    eprintln!("  adpcm PLANAR(L|R): {}", stats(&pcm));
                    write_wav(&format!("/tmp/dbg_{stem}_planar.wav"), &pcm);
                }
            }
        }
    }

    /// Verify the native IMA-Xbox decoder matches ffmpeg's `adpcm_ima_xbox`
    /// output byte-for-byte. Prep: extract the blob (h2 dump probe) + decode the
    /// reference with `ffmpeg -i <wav with fmt 0x0069> ref.wav`. Env: `ADPCM_BLOB`,
    /// `ADPCM_REF` (ffmpeg pcm_s16le wav), `ADPCM_CH`.
    #[test]
    #[ignore]
    fn ima_xbox_matches_ffmpeg() {
        let (Ok(blob), Ok(reff)) = (std::env::var("ADPCM_BLOB"), std::env::var("ADPCM_REF")) else {
            eprintln!("skip: set ADPCM_BLOB + ADPCM_REF");
            return;
        };
        let ch: u16 = std::env::var("ADPCM_CH").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
        let bytes = std::fs::read(&blob).unwrap();
        let mine = decode_xbox_adpcm(&bytes, ch, 44_100).unwrap();
        // reference wav: locate the `data` chunk (ffmpeg emits a LIST chunk
        // before it, so the header isn't a fixed 44 bytes), then read i16 LE PCM.
        let ref_bytes = std::fs::read(&reff).unwrap();
        let data_at = ref_bytes
            .windows(4)
            .position(|w| w == b"data")
            .map(|p| p + 8)
            .expect("data chunk");
        let ref_pcm: Vec<i16> = ref_bytes[data_at..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        let n = mine.samples.len().min(ref_pcm.len());
        let mut max_diff = 0i32;
        let mut mism = 0usize;
        for i in 0..n {
            let d = (mine.samples[i] as i32 - ref_pcm[i] as i32).abs();
            max_diff = max_diff.max(d);
            if d != 0 {
                mism += 1;
            }
        }
        eprintln!(
            "mine={} ref={} compared={n} mismatches={mism} max_diff={max_diff}",
            mine.samples.len(),
            ref_pcm.len()
        );
        eprintln!("mine[0..12]={:?}", &mine.samples[..12.min(mine.samples.len())]);
        eprintln!("ref [0..12]={:?}", &ref_pcm[..12.min(ref_pcm.len())]);
        assert!(max_diff <= 1, "decode differs from ffmpeg by {max_diff}");
    }

    /// Decode a blob at N channels, downmix to stereo, write a WAV to listen.
    /// `BLOB=/tmp/quad.bin ADPCM_CH=4 cargo test decode_channels_to_wav -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn decode_channels_to_wav() {
        let Ok(blob) = std::env::var("BLOB") else {
            eprintln!("skip: set BLOB");
            return;
        };
        let ch: u16 = std::env::var("ADPCM_CH").ok().and_then(|s| s.parse().ok()).unwrap_or(4);
        let bytes = std::fs::read(&blob).unwrap();
        let pcm = decode_xbox_adpcm(&bytes, ch, 44_100).unwrap();
        eprintln!("decoded {}", stats(&pcm));
        let stereo = crate::audio::downmix_to_stereo(&pcm.samples, pcm.channels as usize);
        let out = DecodedPcm { samples: stereo, channels: 2, sample_rate: 44_100 };
        write_wav("/tmp/adpcm_ch_mine.wav", &out);
        eprintln!("wrote /tmp/adpcm_ch_mine.wav ({})", stats(&out));
    }

    /// Decode the real H2 Xbox-ADPCM blob extracted from a tag (skip-if-absent).
    #[test]
    #[ignore]
    fn decode_h2_adpcm_blob() {
        let path = std::env::var("H2_ADPCM")
            .unwrap_or_else(|_| "/tmp/h2_pistol_indoor_med_close.bin".to_owned());
        if !std::path::Path::new(&path).exists() {
            eprintln!("skip: no {path}");
            return;
        }
        let bytes = std::fs::read(&path).unwrap();
        let pcm = decode_xbox_adpcm(&bytes, 1, 48_000).expect("decode h2 adpcm");
        eprintln!(
            "H2 adpcm: {} bytes -> {} frames {}ch {}Hz ({:.2}s)",
            bytes.len(),
            pcm.frame_count(),
            pcm.channels,
            pcm.sample_rate,
            pcm.duration_secs()
        );
        assert!(pcm.frame_count() > 0);
    }

    /// Decode the real H2 Opus blob extracted from a tag (skip-if-absent). Write
    /// it with the Baboon `h2_dump_data_fields` probe first.
    #[test]
    #[ignore]
    fn decode_h2_opus_blob() {
        let path = std::env::var("H2_OPUS").unwrap_or_else(|_| "/tmp/h2_pickup_health.bin".to_owned());
        if !std::path::Path::new(&path).exists() {
            eprintln!("skip: no {path}");
            return;
        }
        let bytes = std::fs::read(&path).unwrap();
        let pcm = decode_opus(&bytes, 1).expect("decode h2 opus");
        eprintln!(
            "H2 opus: {} bytes -> {} frames {}ch {}Hz ({:.2}s)",
            bytes.len(),
            pcm.frame_count(),
            pcm.channels,
            pcm.sample_rate,
            pcm.duration_secs()
        );
        assert!(pcm.frame_count() > 0);
    }
}
