//! Scan all levels/multi + levels/dlc scenarios → their referenced
//! `sky_atm_parameters` tags → the weather `.effect` tags those reference,
//! and dump every effect event's delay/duration bounds + parallel flag.
//! Answers: is the ash `0..0` duration universal across weather effects?
use blam_tags::api::TagStruct;
use blam_tags::effect::{EffectDefinition, EffectDefinitionFlags};
use blam_tags::fields::{TagFieldData, TagFieldType};
use blam_tags::TagFile;
use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

fn collect_refs(s: &TagStruct, out: &mut Vec<String>) {
    for f in s.fields() {
        match f.field_type() {
            TagFieldType::TagReference => {
                if let Some(TagFieldData::TagReference(r)) = f.value() {
                    if let Some((_g, name)) = r.group_tag_and_name {
                        if !name.is_empty() {
                            out.push(name);
                        }
                    }
                }
            }
            TagFieldType::Struct => {
                if let Some(sub) = f.as_struct() { collect_refs(&sub, out); }
            }
            TagFieldType::Block => {
                if let Some(b) = f.as_block() { for el in b.iter() { collect_refs(&el, out); } }
            }
            TagFieldType::Array => {
                if let Some(a) = f.as_array() { for el in a.iter() { collect_refs(&el, out); } }
            }
            _ => {}
        }
    }
}

fn scenarios_in(root: &str, sub: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() { walk(&p, files); }
                else if p.extension().map_or(false, |x| x == "scenario") { files.push(p); }
            }
        }
    }
    walk(&Path::new(root).join(sub), &mut files);
    files.sort();
    files
}

/// Collect refs from a tag, keeping only those whose `{root}/{name}.{ext}`
/// file exists (resolves the group without needing fourccs).
fn refs_of_ext(root: &str, tag_path: &Path, ext: &str) -> Vec<String> {
    let res = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let tag = TagFile::read(tag_path).ok()?;
        let mut refs = Vec::new();
        collect_refs(&tag.root(), &mut refs);
        Some(refs)
    }));
    let Ok(Some(refs)) = res else { return Vec::new() };
    let mut out = BTreeSet::new();
    for name in refs {
        let name = name.replace('\\', "/"); // tag paths are Windows-style
        if Path::new(root).join(format!("{name}.{ext}")).exists() {
            out.insert(name);
        }
    }
    out.into_iter().collect()
}

fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let root = "/Users/camden/Halo/halo3_mcc/tags";

    let mut weather_effects: BTreeSet<String> = BTreeSet::new();
    let mut skies: BTreeSet<String> = BTreeSet::new();

    for sub in ["levels/multi", "levels/dlc"] {
        println!("\n======== {sub} ========");
        for scn in scenarios_in(root, sub) {
            let name = scn.strip_prefix(root).unwrap_or(&scn).with_extension("");
            let sky_refs = refs_of_ext(root, &scn, "sky_atm_parameters");
            if sky_refs.is_empty() { continue; }
            println!("  {}", name.display());
            for sky in &sky_refs {
                skies.insert(sky.clone());
                let sky_path = Path::new(root).join(format!("{sky}.sky_atm_parameters"));
                let effs = refs_of_ext(root, &sky_path, "effect");
                println!("    sky: {sky}  -> {} weather effect(s)", effs.len());
                for e in effs {
                    println!("        {e}");
                    weather_effects.insert(e);
                }
            }
        }
    }

    println!("\n======== WEATHER EFFECT EVENT TIMING ({} unique) ========", weather_effects.len());
    let mut all_zero = 0;
    let mut any_nonzero = 0;
    for eff in &weather_effects {
        let path = Path::new(root).join(format!("{eff}.effect"));
        let res = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let tag = TagFile::read(&path).ok()?;
            EffectDefinition::from_tag(&tag).ok()
        }));
        let Ok(Some(def)) = res else { println!("  {eff}: <parse failed>"); continue; };
        let parallel = def.flags.contains(EffectDefinitionFlags::RunEventsInParallel);
        let zero = def.events.iter().all(|e| {
            e.delay_bounds.lower == 0.0 && e.delay_bounds.upper == 0.0
                && e.duration_bounds.lower == 0.0 && e.duration_bounds.upper == 0.0
        });
        if zero { all_zero += 1; } else { any_nonzero += 1; }
        println!("  {eff}  [parallel={parallel}, events={}, loop_start={}]  {}",
            def.events.len(), def.loop_start_event, if zero { "*** ALL 0..0 ***" } else { "has timing" });
        for (i, ev) in def.events.iter().enumerate() {
            println!("      ev[{i}] delay={:.4}..{:.4} duration={:.4}..{:.4}",
                ev.delay_bounds.lower, ev.delay_bounds.upper,
                ev.duration_bounds.lower, ev.duration_bounds.upper);
        }
    }
    println!("\nSUMMARY: {all_zero} weather effects ALL-0..0, {any_nonzero} with nonzero timing, {} skies", skies.len());
}
