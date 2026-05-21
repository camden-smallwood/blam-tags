//! Dump an `effect_globals` (`effg`) tag — the 28 per-component
//! holdback budgets × 3 priorities consumed by `effect_allocate`.

use blam_tags::effect_globals::{EffectGlobals, EffectPriority};
use blam_tags::file::TagFile;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        "/Users/camden/Halo/halo3_mcc/tags/globals/effect_globals.effect_globals".into()
    });
    let tag = TagFile::read(&path).expect("failed to read tag");
    let eg = EffectGlobals::from_tag(&tag).expect("failed to parse effg");

    println!("=== {} ===", path);
    println!("holdbacks: {}", eg.holdbacks.len());
    println!();
    // Three columns per priority (norm/high/essential):
    //   abs: authored absolute count
    //   rel: authored relative percentage
    //   avl: tool.exe-resolved runtime "available" count (zero in
    //         extracted source tags; runtime caches carry the real value)
    println!(
        "{:22} {:>5} | {:>5} {:>5} {:>5} | {:>5} {:>5} {:>5} | {:>5} {:>5} {:>5}",
        "type", "ovrll",
        "n.abs", "n.rel", "n.avl",
        "h.abs", "h.rel", "h.avl",
        "e.abs", "e.rel", "e.avl",
    );
    println!("{}", "-".repeat(110));
    for h in &eg.holdbacks {
        let name = match h.holdback_type {
            Some(t) => format!("{:?}", t),
            None => "?".into(),
        };
        let p = |pr: EffectPriority| -> (i32, f32, i32) {
            h.priorities
                .iter()
                .find(|x| x.priority == Some(pr))
                .map(|x| (x.absolute_count, x.relative_percentage, x.available))
                .unwrap_or((0, 0.0, 0))
        };
        let (na, nr, nv) = p(EffectPriority::Normal);
        let (ha, hr, hv) = p(EffectPriority::High);
        let (ea, er, ev) = p(EffectPriority::Essential);
        println!(
            "{:22} {:>5} | {:>5} {:>5.2} {:>5} | {:>5} {:>5.2} {:>5} | {:>5} {:>5.2} {:>5}",
            name, h.overall_budget,
            na, nr, nv,
            ha, hr, hv,
            ea, er, ev,
        );
    }
}
