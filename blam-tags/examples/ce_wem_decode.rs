//! Decode Campaign Evolved Wwise media straight out of the legacy `.pak`
//! containers, to prove the existing `audio::wwise::decode_wem` path (built for
//! Halo 4 / H2A) works unchanged on CE's `.wem` files.
//!
//! Run:
//!   cargo run -p blam-tags --features "iostore audio" --example ce_wem_decode -- \
//!     <pak> <substr> [step]

#[path = "ce_pak_extract.rs"]
mod pak;

use blam_tags::iostore::pak::PakArchive;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let pak_path = args.next().expect("usage: <pak> <substr> [step]");
    let substr = args.next().unwrap_or_else(|| ".wem".into()).to_ascii_lowercase();
    let step: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);

    let mut p = PakArchive::open(&pak_path)?;
    let hits: Vec<String> = p
        .files()
        .iter()
        .filter(|f| f.mounted_path.to_ascii_lowercase().contains(&substr))
        .map(|f| f.mounted_path.clone())
        .collect();
    println!("matches: {}", hits.len());

    let (mut ok, mut fail) = (0usize, 0usize);
    let mut errs: std::collections::BTreeMap<String, usize> = Default::default();
    let mut shown = 0usize;

    for k in hits.iter().step_by(step) {
        let data = match p.read(k) {
            Ok(d) => d,
            Err(e) => {
                *errs.entry(format!("pak: {e}")).or_default() += 1;
                fail += 1;
                continue;
            }
        };
        match blam_tags::audio::wwise::decode_wem(&data) {
            Ok(pcm) => {
                ok += 1;
                if shown < 6 {
                    shown += 1;
                    let frames = pcm.samples.len() / pcm.channels.max(1) as usize;
                    println!(
                        "  {k}\n     -> {} ch, {} Hz, {} samples ({:.2}s)  [{}]",
                        pcm.channels,
                        pcm.sample_rate,
                        pcm.samples.len(),
                        frames as f32 / pcm.sample_rate as f32,
                        pak::describe_wem(&data),
                    );
                }
            }
            Err(e) => {
                fail += 1;
                *errs.entry(e).or_default() += 1;
            }
        }
    }

    println!("\ndecoded ok: {ok}   failed: {fail}");
    let mut rows: Vec<_> = errs.into_iter().collect();
    rows.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    for (e, c) in rows.iter().take(10) {
        println!("  {c:>6}  {e}");
    }
    Ok(())
}
