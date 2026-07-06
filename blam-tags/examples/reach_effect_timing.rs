use blam_tags::effect::EffectDefinition;
use blam_tags::TagFile;

fn main() {
    let tags = [
        "/Users/camden/Halo/haloreach_mcc/tags/cinematics/010la_outpost/fx/takeoff_dust.effect",
        "/Users/camden/Halo/haloreach_mcc/tags/multiplayer/sandbox/cursor_impact.effect",
    ];
    for path in tags {
        println!("\n#### {} ####", path.rsplit('/').next().unwrap());
        let r = std::panic::catch_unwind(|| {
            let tag = TagFile::read(path)?;
            let def = EffectDefinition::from_tag(&tag)?;
            Ok::<_, Box<dyn std::error::Error>>(def)
        });
        match r {
            Ok(Ok(def)) => {
                println!("  parallel={} loop_start={} events={}",
                    def.flags.contains(blam_tags::effect::EffectDefinitionFlags::RunEventsInParallel),
                    def.loop_start_event, def.events.len());
                for (i, ev) in def.events.iter().enumerate() {
                    println!("  event[{i}]: delay={:.4}..{:.4} duration={:.4}..{:.4}",
                        ev.delay_bounds.lower, ev.delay_bounds.upper,
                        ev.duration_bounds.lower, ev.duration_bounds.upper);
                }
            }
            Ok(Err(e)) => println!("  decode error: {e}"),
            Err(_) => println!("  *** PANIC during decode (mela/type-mismatch?) ***"),
        }
    }
}
