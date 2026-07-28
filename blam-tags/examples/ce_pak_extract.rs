//! Inspect the UE5 *legacy* `.pak` containers a Campaign Evolved build stages
//! its Wwise audio in (chunk0 = non-localized SFX, chunk1..13 = per-language
//! voice), using `blam_tags::iostore::pak`.
//!
//! Modes:
//!   list    (default) show matching entries, extract the first few
//!   survey  decode every match far enough to histogram the `.wem` codecs
//!
//! Run:
//!   cargo run --release -p blam-tags --features iostore --example ce_pak_extract -- \
//!     <pak> <substr> [outdir]
//!   PAK_SURVEY=1 PAK_STEP=17 cargo run ... -- <pak> .wem

use std::collections::BTreeMap;

use blam_tags::iostore::pak::PakArchive;

/// Name the codec of a Wwise `.wem` — a RIFF whose `fmt ` tag identifies an
/// Audiokinetic codec rather than a Microsoft one — plus its rate/channels.
pub fn describe_wem(d: &[u8]) -> String {
    if d.len() < 12 || &d[0..4] != b"RIFF" {
        return "not-RIFF".into();
    }
    let mut o = 12usize;
    while o + 8 <= d.len() {
        let id = &d[o..o + 4];
        let sz = u32::from_le_bytes(d[o + 4..o + 8].try_into().unwrap()) as usize;
        if id == b"fmt " && o + 8 + sz.min(16) <= d.len() {
            let f = &d[o + 8..];
            let tag = u16::from_le_bytes(f[0..2].try_into().unwrap());
            let ch = u16::from_le_bytes(f[2..4].try_into().unwrap());
            let rate = u32::from_le_bytes(f[4..8].try_into().unwrap());
            let name = match tag {
                0x0001 => "PCM",
                0xFFFE => "PCM (extensible)",
                0x0002 => "ADPCM(MS)",
                0x0166 => "XMA2",
                0xFFFF => "Wwise Vorbis",
                0x0069 => "IMA ADPCM",
                0x3039 | 0x3040 | 0x3041 => "Wwise Opus",
                _ => "unknown",
            };
            return format!("{name} [0x{tag:04x}] {rate}Hz {ch}ch");
        }
        o += 8 + sz + (sz & 1);
    }
    "no-fmt".into()
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let pak = args.next().expect("usage: <pak> <substr> [outdir]");
    let substr = args.next().unwrap_or_default().to_ascii_lowercase();
    let outdir = args.next();

    let mut p = PakArchive::open(&pak)?;
    println!(
        "mount: {}  methods: {:?}  entries: {}",
        p.mount_point(),
        p.methods(),
        p.files().len()
    );

    let hits: Vec<String> = p
        .files()
        .iter()
        .filter(|f| f.mounted_path.to_ascii_lowercase().contains(&substr))
        .map(|f| f.mounted_path.clone())
        .collect();
    println!("matches: {}", hits.len());

    if std::env::var("PAK_SURVEY").is_ok() {
        let step: usize =
            std::env::var("PAK_STEP").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
        let mut hist: BTreeMap<String, usize> = BTreeMap::new();
        let (mut n, mut failed) = (0usize, 0usize);
        for k in hits.iter().step_by(step) {
            match p.read(k) {
                Ok(d) => {
                    n += 1;
                    *hist.entry(describe_wem(&d)).or_default() += 1;
                }
                Err(_) => failed += 1,
            }
        }
        println!("\nsurveyed {n} files (every {step}), {failed} failed");
        let mut rows: Vec<_> = hist.into_iter().collect();
        rows.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        for (k, c) in rows {
            println!("  {c:>7}  {k}");
        }
        return Ok(());
    }

    for k in hits.iter().take(8) {
        let data = p.read(k)?;
        println!("\n{k}  ({} bytes)  {}", data.len(), describe_wem(&data));
        for row in data[..data.len().min(64)].chunks(16) {
            let hex: String = row.iter().map(|b| format!("{b:02x} ")).collect();
            let asc: String = row
                .iter()
                .map(|&b| if (32..127).contains(&b) { b as char } else { '.' })
                .collect();
            println!("  {hex:<48} {asc}");
        }
        if let Some(dir) = &outdir {
            std::fs::create_dir_all(dir)?;
            let name = k.rsplit('/').next().unwrap();
            std::fs::write(format!("{dir}/{name}"), &data)?;
            println!("  -> wrote {dir}/{name}");
        }
    }
    Ok(())
}
