//! How much of the game's own reflection namespace is missing from the `.usmap`?
//!
//! The shipped `global.utoc` carries a script-object table naming every engine
//! object the game can load. The `.usmap` is a *dump* of the same reflection
//! data taken from a running process, so anything the dumper did not see is
//! simply absent. Comparing the two says exactly which modules the dump missed,
//! which is the difference between "no schema exists" and "our dump is short".
use std::collections::{BTreeMap, BTreeSet};

use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::usmap::Usmap;

const GLOBAL: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks/global.utoc";
const USMAP: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap");

fn main() {
    let usmap = Usmap::parse(&std::fs::read(USMAP).unwrap()).unwrap();
    let known: BTreeSet<&str> = usmap.structs.iter().map(|s| s.name.as_str()).collect();
    let so = ScriptObjects::load(GLOBAL).expect("load script objects");

    // Direct children of a `/Script/Module` package are its classes/structs/enums.
    let mut per_module: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for e in so.entries() {
        let Some(path) = so.resolve(e.global_index.raw_index()) else { continue };
        let Some(rest) = path.strip_prefix("/Script/") else { continue };
        // "Module.Name" only — skip deeper members ("Module.Class:Function").
        let Some((module, name)) = rest.split_once('.') else { continue };
        if name.contains(':') || name.contains('.') {
            continue;
        }
        let slot = per_module.entry(module.to_string()).or_default();
        slot.0 += 1;
        if known.contains(name) {
            slot.1 += 1;
        }
    }

    let mut missing: Vec<_> =
        per_module.iter().filter(|(_, (t, k))| *k < *t).map(|(m, (t, k))| (t - k, t, m)).collect();
    missing.sort_by_key(|(gap, _, _)| std::cmp::Reverse(*gap));

    let total: usize = per_module.values().map(|(t, _)| t).sum();
    let covered: usize = per_module.values().map(|(_, k)| k).sum();
    println!("script-object entries named `/Script/Module.Name`: {total}");
    println!("  present in the .usmap: {covered} ({:.2}%)", 100.0 * covered as f64 / total as f64);
    println!("  absent:                {}", total - covered);
    println!("\nmodules the dump is missing entries for (gap / total / module):");
    for (gap, t, m) in missing.iter().take(30) {
        println!("  {gap:>5} / {t:>5}  {m}");
    }
    println!("\nfully-absent modules: {}", missing.iter().filter(|(g, t, _)| *g == **t).count());
}
