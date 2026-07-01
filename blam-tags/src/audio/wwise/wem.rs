//! Wwise Encoded Media (`.wem`) parsing + decode to interleaved PCM.
//!
//! A `.wem` is a RIFF/WAVE whose `fmt ` codec tag selects the encoding. Halo 4
//! (MCC/PC) uses two:
//! - `0xFFFF` — Wwise-Vorbis (97% of media); rebuilt to standard Ogg by
//!   [`super::ww_vorbis`] and decoded with `lewton`.
//! - `0xFFFE` — WAVEFORMATEXTENSIBLE PCM (the rest); interleaved 16-bit.
//!
//! XMA2 / xWMA (`0x0165`/`0x0166`) do not appear in the MCC repackage, so they
//! are reported as unsupported rather than decoded.

use super::super::vorbis::{decode_ogg_vorbis, DecodedPcm};
use super::ww_vorbis::WwiseVorbis;

fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Located RIFF chunks within a `.wem` (offsets to chunk *bodies*).
struct WemChunks {
    fmt: (usize, usize),
    data: (usize, usize),
}

fn find_chunks(wem: &[u8]) -> Result<WemChunks, String> {
    if wem.len() < 12 || &wem[0..4] != b"RIFF" || &wem[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE .wem".into());
    }
    let riff_end = (rd_u32(wem, 4) as usize + 8).min(wem.len());
    let mut fmt = None;
    let mut data = None;
    let mut p = 12;
    while p + 8 <= riff_end {
        let id = &wem[p..p + 4];
        let size = rd_u32(wem, p + 4) as usize;
        let body = p + 8;
        let end = (body + size).min(wem.len());
        match id {
            b"fmt " => fmt = Some((body, end)),
            b"data" => data = Some((body, end)),
            _ => {}
        }
        p = body + size + (size & 1); // chunks are word-aligned
    }
    Ok(WemChunks {
        fmt: fmt.ok_or("no fmt chunk")?,
        data: data.ok_or("no data chunk")?,
    })
}

/// Decode a `.wem`'s bytes to interleaved 16-bit PCM.
pub fn decode_wem(wem: &[u8]) -> Result<DecodedPcm, String> {
    let chunks = find_chunks(wem)?;
    let (fmt_start, fmt_end) = chunks.fmt;
    let fmt = &wem[fmt_start..fmt_end];
    let fmt_size = fmt.len();
    let codec = rd_u16(fmt, 0);
    let channels = rd_u16(fmt, 2) as u32;
    let sample_rate = rd_u32(fmt, 4);
    let avg_bytes_per_second = rd_u32(fmt, 8);

    let (data_start, data_end) = chunks.data;
    let data = &wem[data_start..data_end];

    match codec {
        0xFFFF => {
            // Wwise-Vorbis. H4 has no separate `vorb` chunk; the vorb fields
            // live inside the 0x42-byte fmt at fmt+0x18 (ww2ogg "fake it out").
            if fmt_size != 0x42 {
                return Err(format!(
                    "unsupported Wwise-Vorbis fmt size 0x{fmt_size:x} (expected 0x42)"
                ));
            }
            let vorb = 0x18;
            let mod_signal = rd_u32(fmt, vorb + 0x04);
            let mod_packets =
                !matches!(mod_signal, 0x4A | 0x4B | 0x69 | 0x70);
            let setup_packet_offset = rd_u32(fmt, vorb + 0x10);
            let first_audio_packet_offset = rd_u32(fmt, vorb + 0x14);
            let blocksize_0_pow = fmt[vorb + 0x28];
            let blocksize_1_pow = fmt[vorb + 0x29];

            let rebuilt = WwiseVorbis {
                data,
                channels,
                sample_rate,
                avg_bytes_per_second,
                setup_packet_offset,
                first_audio_packet_offset,
                blocksize_0_pow,
                blocksize_1_pow,
                mod_packets,
            }
            .to_ogg()?;
            decode_ogg_vorbis(&rebuilt)
        }
        0xFFFE => {
            // WAVEFORMATEXTENSIBLE PCM. bits==0 → assume 16-bit.
            let bits = rd_u16(fmt, 14);
            if bits != 0 && bits != 16 {
                return Err(format!("unsupported PCM bit depth {bits}"));
            }
            let mut samples = Vec::with_capacity(data.len() / 2);
            for s in data.chunks_exact(2) {
                samples.push(i16::from_le_bytes([s[0], s[1]]));
            }
            Ok(DecodedPcm {
                samples,
                channels: channels as u16,
                sample_rate,
            })
        }
        other => Err(format!("unsupported .wem codec 0x{other:04x}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Set `WWISE_WEM=/path/to/file.wem` (extract with the pck/bnk readers or
    /// dump from a .pck). Optionally `WWISE_WEM_REF=/path/ref.wav` (a
    /// `vgmstream-cli -o ref.wav file.wem`) to check sample parity.
    #[test]
    fn decodes_wwise_vorbis_wem() {
        let Some(path) = std::env::var_os("WWISE_WEM").map(PathBuf::from) else {
            eprintln!("WWISE_WEM unset; skipping");
            return;
        };
        let wem = std::fs::read(&path).expect("read wem");
        let pcm = decode_wem(&wem).expect("decode wem");
        eprintln!(
            "{}: {} ch, {} Hz, {} frames ({:.3}s)",
            path.display(),
            pcm.channels,
            pcm.sample_rate,
            pcm.frame_count(),
            pcm.duration_secs()
        );
        assert!(pcm.frame_count() > 0);

        if let Some(refpath) = std::env::var_os("WWISE_WEM_REF").map(PathBuf::from) {
            let wav = std::fs::read(&refpath).expect("read ref wav");
            let ref_frames = ref_wav_frames(&wav);
            let diff = (pcm.frame_count() as i64 - ref_frames as i64).abs();
            eprintln!("ref frames {ref_frames}, ours {}, diff {diff}", pcm.frame_count());
            // Vorbis decoders may differ by a small edge/padding amount.
            assert!(diff <= 2048, "frame count diverges from vgmstream reference");
        }
    }

    /// Sweep every `.wem` in a `.pck` (embedded bank media *and* streamed) and
    /// decode it, tallying hard errors vs zero-frame results by codec.
    /// `WWISE_PCK=/path/to/sfxbank.pck` (or sfxstream.pck). This is the broad
    /// coverage check: does the bundled codebook library decode every
    /// codebook-id variant the game uses, with no parse/rebuild errors?
    ///
    /// A handful of ultra-short (<10ms) single-packet sounds legitimately yield
    /// zero PCM frames — a standard Vorbis decoder needs a second packet to
    /// produce overlap-add output; even ww2ogg's own Ogg behaves the same. So
    /// the assertion is "no hard errors", not "every wem has frames".
    #[test]
    #[ignore]
    fn sweep_all_embedded_wems() {
        use super::super::bnk::Bnk;
        use super::super::pck::Pck;
        use std::collections::BTreeMap;

        let Some(path) = std::env::var_os("WWISE_PCK").map(PathBuf::from) else {
            eprintln!("WWISE_PCK unset; skipping");
            return;
        };
        let pck = Pck::open(&path).expect("open pck");
        let mut ok = 0usize;
        let mut zero_frame = 0usize;
        let mut errors: BTreeMap<String, usize> = BTreeMap::new();
        let mut total = 0usize;

        let mut decode = |wem: &[u8], errors: &mut BTreeMap<String, usize>| {
            match decode_wem(wem) {
                Ok(pcm) if pcm.frame_count() > 0 => ok += 1,
                Ok(_) => zero_frame += 1,
                Err(e) => {
                    let key = e.split(['(', ':']).next().unwrap_or(&e).trim().to_owned();
                    *errors.entry(key).or_default() += 1;
                }
            }
            total += 1;
        };

        // Embedded media (bank DATA chunks).
        for entry in &pck.banks {
            let bytes = pck.read_entry(entry).expect("read bank");
            let Ok(bnk) = Bnk::parse(bytes) else { continue };
            for (_id, wem) in bnk.embedded_wems() {
                decode(wem, &mut errors);
            }
        }
        // Streamed media (loose .wem in the stream section).
        for entry in &pck.streams {
            let wem = pck.read_entry(entry).expect("read stream");
            decode(&wem, &mut errors);
        }

        eprintln!("decoded {ok}/{total} wem ({zero_frame} zero-frame <10ms blips)");
        for (k, v) in &errors {
            eprintln!("  ERROR [{k}]: {v}");
        }
        assert!(errors.is_empty(), "some wem failed to decode with a hard error");
    }

    fn ref_wav_frames(wav: &[u8]) -> usize {
        // locate `data` chunk, divide by frame size from fmt.
        let mut p = 12;
        let mut channels = 1usize;
        let mut bits = 16usize;
        let mut data_len = 0usize;
        while p + 8 <= wav.len() {
            let id = &wav[p..p + 4];
            let size = rd_u32(wav, p + 4) as usize;
            let body = p + 8;
            if id == b"fmt " {
                channels = rd_u16(wav, body + 2) as usize;
                bits = rd_u16(wav, body + 14) as usize;
            } else if id == b"data" {
                data_len = size;
            }
            p = body + size + (size & 1);
        }
        let frame = channels * (bits / 8).max(1);
        if frame == 0 { 0 } else { data_len / frame }
    }
}
