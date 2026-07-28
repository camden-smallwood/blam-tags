//! End-to-end Campaign Evolved sound resolution: a `sound` tag has no sample
//! data of its own (unlike every previous Blam engine), so playback has to walk
//! out of the tag and into Wwise:
//!
//!   Tags/sound/<path>-sound.uasset
//!     -> /Game/Audio/<path>/<name>[_player_variant]      (BlamAudioSound[Combiner])
//!        -> /Game/Audio/.../<name>_player | _non-player  (BlamAudioSound)
//!           -> /Game/Wwise/Events/Play_<event>           (AkAudioEvent)
//!              -> Media/<nn>/<id>.wem  +  <bank>.bnk     (names in the event's name map)
//!
//! The media itself is *not* in IoStore: it is staged loose in the legacy
//! `.pak` containers (chunk0 = non-localized SFX, chunk1..13 = per-language
//! voice), which is why `Pak` below exists.
//!
//! Run:
//!   cargo run --release -p blam-tags --features "iostore audio" --example ce_sound_resolve -- \
//!     <tag-substr> [outdir] [language]

use blam_tags::iostore::pak::PakSet;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

/// Every cooked package in the build, keyed by lowercase `/Game/...` package
/// name, so imports can be followed by name.
struct PackageIndex {
    archives: Vec<IoStoreArchive>,
    /// package name (lowercase) -> (archive index, entry path)
    by_package: BTreeMap<String, (usize, String)>,
}

impl PackageIndex {
    fn build() -> anyhow::Result<Self> {
        let mut utocs: Vec<_> = std::fs::read_dir(PAKS)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
            .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
            .collect();
        utocs.sort();

        let mut archives = Vec::new();
        let mut by_package = BTreeMap::new();
        for utoc in &utocs {
            let Ok(a) = IoStoreArchive::open(utoc) else { continue };
            let ai = archives.len();
            for e in a.entries() {
                let p = e.path.replace('\\', "/");
                if !p.to_ascii_lowercase().ends_with(".uasset") {
                    continue;
                }
                // Cooked path -> package name: Meteorite/Content/X.uasset => /Game/X
                let Some(rest) = p.split_once("/Content/").map(|(_, r)| r) else { continue };
                let stem = rest.trim_end_matches(".uasset").trim_end_matches(".uasset");
                let pkg = format!("/Game/{stem}").to_ascii_lowercase();
                by_package.entry(pkg).or_insert((ai, e.path.clone()));
            }
            archives.push(a);
        }
        Ok(Self { archives, by_package })
    }

    fn header(&self, pkg: &str) -> Option<(FZenPackageHeader, Vec<u8>)> {
        let (ai, path) = self.by_package.get(&pkg.to_ascii_lowercase())?;
        let bytes = self.archives[*ai].read(path).ok()?;
        let mut cur = Cursor::new(&bytes);
        let hdr = FZenPackageHeader::deserialize(&mut cur, None, CV, HV, None).ok()?;
        Some((hdr, bytes))
    }

    fn sound_tags(&self) -> impl Iterator<Item = &String> {
        self.by_package.keys().filter(|k| k.starts_with("/game/tags/sound/"))
    }

    /// Prefer a tag that actually binds to Wwise — plenty of `sound` tags are
    /// unbound stubs with no imports at all.
    fn find_tag(&self, substr: &str) -> Option<String> {
        let s = substr.to_ascii_lowercase();
        let hits: Vec<&String> = self.sound_tags().filter(|k| k.contains(&s)).collect();
        let bound = hits.iter().find(|k| {
            self.header(k)
                .is_some_and(|(h, _)| h.imported_package_names.iter().any(|i| {
                    i.to_ascii_lowercase().starts_with("/game/audio/")
                }))
        });
        bound.or(hits.first()).map(|s| (*s).clone())
    }
}

/// What an `AkAudioEvent` package tells us, read straight off its name map.
#[derive(Debug, Default)]
struct EventInfo {
    event: Option<String>,
    banks: Vec<String>,
    media: Vec<String>,
    sources: Vec<String>,
    languages: Vec<String>,
}

const LANGUAGES: &[&str] = &[
    "SFX", "English(US)", "English(UK)", "French(France)", "German", "Italian",
    "Spanish(Spain)", "Spanish(Mexico)", "Japanese", "Korean", "Polish",
    "Portuguese(Brazil)", "Russian", "Chinese(Simplified)", "Chinese(Taiwan)",
];

fn read_event(names: &[String]) -> EventInfo {
    let mut info = EventInfo::default();
    for n in names {
        if n.to_ascii_lowercase().ends_with(".bnk") {
            info.banks.push(n.clone());
        } else if n.to_ascii_lowercase().ends_with(".wem") {
            info.media.push(n.clone());
        } else if n.to_ascii_lowercase().ends_with(".wav") {
            info.sources.push(n.clone());
        } else if LANGUAGES.iter().any(|l| l.eq_ignore_ascii_case(n)) {
            info.languages.push(n.clone());
        } else if n.starts_with("Play_") && !n.contains('/') {
            // The Wwise-side event name keeps its authored casing; the UE asset
            // name is lowercased, so prefer the one that isn't all-lowercase.
            if info.event.as_deref().is_none_or(|e| e == &e.to_ascii_lowercase()) {
                info.event = Some(n.clone());
            }
        }
    }
    info
}

fn write_wav(path: &str, pcm: &blam_tags::audio::DecodedPcm) -> anyhow::Result<()> {
    let ch = pcm.channels as u16;
    let rate = pcm.sample_rate;
    let bits = 16u16;
    let data_len = (pcm.samples.len() * 2) as u32;
    let mut o: Vec<u8> = Vec::with_capacity(44 + data_len as usize);
    o.extend_from_slice(b"RIFF");
    o.extend_from_slice(&(36 + data_len).to_le_bytes());
    o.extend_from_slice(b"WAVEfmt ");
    o.extend_from_slice(&16u32.to_le_bytes());
    o.extend_from_slice(&1u16.to_le_bytes());
    o.extend_from_slice(&ch.to_le_bytes());
    o.extend_from_slice(&rate.to_le_bytes());
    o.extend_from_slice(&(rate * ch as u32 * 2).to_le_bytes());
    o.extend_from_slice(&(ch * 2).to_le_bytes());
    o.extend_from_slice(&bits.to_le_bytes());
    o.extend_from_slice(b"data");
    o.extend_from_slice(&data_len.to_le_bytes());
    for s in &pcm.samples {
        o.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, o)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let want = args.next().expect("usage: <tag-substr> [outdir] [language]");
    let outdir = args.next();
    let language = args.next().unwrap_or_else(|| "English(US)".into());

    eprintln!("indexing packages...");
    let idx = PackageIndex::build()?;
    eprintln!("indexed {} packages", idx.by_package.len());

    // `sweep` mode: how many sound tags actually bind to Wwise at all?
    if want == "--sweep" {
        let (mut bound, mut stub, mut total) = (0usize, 0usize, 0usize);
        for t in idx.sound_tags().cloned().collect::<Vec<_>>() {
            total += 1;
            let Some((h, _)) = idx.header(&t) else { continue };
            if h.imported_package_names
                .iter()
                .any(|i| i.to_ascii_lowercase().starts_with("/game/audio/"))
            {
                bound += 1;
            } else {
                stub += 1;
            }
        }
        println!("sound tags: {total}   bound to /Game/Audio: {bound}   unbound stubs: {stub}");
        return Ok(());
    }

    // `coverage` mode: sample tags, resolve them fully, and check that every
    // referenced media file really exists loose in a pak (vs. only inside a
    // .bnk, which would need bank extraction too).
    if want == "--coverage" {
        let step: usize = outdir.as_deref().and_then(|s| s.parse().ok()).unwrap_or(200);
        let set = PakSet::open_dir(PAKS)?;
        let loose: BTreeSet<String> = set
            .paths()
            .filter_map(|f| f.find("Media/").map(|i| f[i..].to_string()))
            .collect();
        eprintln!("loose media entries across {} paks: {}", set.len(), loose.len());

        let tags: Vec<String> = idx.sound_tags().cloned().collect();
        let (mut checked, mut with_media, mut no_event, mut hit, mut miss) = (0, 0, 0, 0usize, 0usize);
        for t in tags.iter().step_by(step) {
            checked += 1;
            let mut seen = BTreeSet::new();
            let mut q = VecDeque::new();
            q.push_back(t.clone());
            let mut media: Vec<String> = Vec::new();
            while let Some(pkg) = q.pop_front() {
                let Some((h, _)) = idx.header(&pkg) else { continue };
                for imp in &h.imported_package_names {
                    let low = imp.to_ascii_lowercase();
                    if !seen.insert(low.clone()) {
                        continue;
                    }
                    if low.starts_with("/game/wwise/events/")
                        || low.starts_with("/game/wwiseaudio/events/")
                    {
                        if let Some((eh, _)) = idx.header(imp) {
                            media.extend(read_event(&eh.name_map.copy_raw_names()).media);
                        }
                    } else if low.starts_with("/game/audio/") {
                        q.push_back(imp.clone());
                    }
                }
            }
            if media.is_empty() {
                no_event += 1;
                continue;
            }
            with_media += 1;
            for m in media {
                if loose.contains(&m) {
                    hit += 1;
                } else {
                    miss += 1;
                    if miss < 8 {
                        println!("  missing loose media: {m}  (tag {t})");
                    }
                }
            }
        }
        println!(
            "\nchecked {checked} tags (every {step}): {with_media} yielded media, \
             {no_event} yielded none\nmedia refs: {hit} present loose, {miss} missing"
        );
        return Ok(());
    }

    let tag = idx.find_tag(&want).ok_or_else(|| anyhow::anyhow!("no sound tag matching {want:?}"))?;
    println!("tag package: {tag}");

    // Walk imports breadth-first out of the tag until we reach event assets.
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::new();
    let mut events: Vec<String> = Vec::new();
    queue.push_back(tag.clone());
    seen.insert(tag.to_ascii_lowercase());

    while let Some(pkg) = queue.pop_front() {
        let Some((hdr, _)) = idx.header(&pkg) else { continue };
        for imp in &hdr.imported_package_names {
            let low = imp.to_ascii_lowercase();
            if !seen.insert(low.clone()) {
                continue;
            }
            // Two event roots: `/Game/Wwise/Events` (SFX) and
            // `/Game/WwiseAudio/Events` (systemic VO / dialogue).
            if low.starts_with("/game/wwise/events/") || low.starts_with("/game/wwiseaudio/events/")
            {
                events.push(imp.clone());
            } else if low.starts_with("/game/audio/") {
                println!("  via {imp}");
                queue.push_back(imp.clone());
            }
        }
    }

    println!("\nresolved {} event(s):", events.len());
    let mut media_wanted: Vec<(String, String)> = Vec::new(); // (media path, label)

    for ev in &events {
        let Some((hdr, _)) = idx.header(ev) else {
            println!("  {ev}  <unreadable>");
            continue;
        };
        let names = hdr.name_map.copy_raw_names();
        let info = read_event(&names);
        println!("\n  {ev}");
        println!("     event      : {}", info.event.as_deref().unwrap_or("?"));
        println!("     language   : {:?}", info.languages);
        println!("     bank(s)    : {:?}", info.banks);
        for (i, m) in info.media.iter().enumerate() {
            let src = info.sources.get(i).map(|s| s.as_str()).unwrap_or("");
            println!("     media[{i}]   : {m}   <- {src}");
            let label = format!(
                "{}_{}",
                info.event.as_deref().unwrap_or("event"),
                m.rsplit('/').next().unwrap().trim_end_matches(".wem")
            );
            media_wanted.push((m.clone(), label));
        }
        if info.media.is_empty() {
            println!("     (no media in name map — streamed or bank-resident)");
        }
    }

    let Some(outdir) = outdir else { return Ok(()) };
    std::fs::create_dir_all(&outdir)?;

    // One namespace across every chunk: mount points differ (chunk0 mounts at
    // the staging root, the language chunks at Content/WwiseAudio), but PakSet
    // normalizes them so a single mounted path resolves wherever it lives.
    let mut set = PakSet::open_dir(PAKS)?;
    println!("\nopened {} pak containers", set.len());

    println!("extracting {} media file(s) (preferred language {language}):", media_wanted.len());
    for (m, label) in &media_wanted {
        let full = format!("Meteorite/Content/WwiseAudio/{m}");
        match set.read(&full) {
            Ok(data) => match blam_tags::audio::wwise::decode_wem(&data) {
                Ok(pcm) => {
                    let out = format!("{outdir}/{label}.wav");
                    write_wav(&out, &pcm)?;
                    let frames = pcm.samples.len() / pcm.channels.max(1) as usize;
                    println!(
                        "  {m}  -> {label}.wav  ({} ch, {} Hz, {:.2}s)",
                        pcm.channels,
                        pcm.sample_rate,
                        frames as f32 / pcm.sample_rate as f32
                    );
                }
                Err(e) => println!("  {m}  decode failed: {e}"),
            },
            Err(e) => println!("  {m}  {e}"),
        }
    }
    Ok(())
}
