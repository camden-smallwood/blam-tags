use blam_tags::effect::{EffectDefinition, EffectFlags, EffectEventFlags};
use blam_tags::TagFile;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = "/Users/camden/Halo/halo3_mcc/tags";
    for eff in ["ash_02", "ash_01"] {
        let path = format!("{root}/fx/scenery_fx/weather/ash/{eff}.effect");
        let tag = TagFile::read(&path)?;
        let def = EffectDefinition::from_tag(&tag)?;
        println!("\n#### {eff}.effect ####");
        println!("  parallel_events           = {}", def.flags.contains(EffectFlags::RunEventsInParallel));
        println!("  do_not_reuse_when_looping = {}", def.flags.contains(EffectFlags::DoNotReusePartsWhenLooping));
        println!("  cannot_be_restarted       = {}", def.flags.contains(EffectFlags::CannotBeRestarted));
        println!("  loop_start_event          = {}", def.loop_start_event);
        println!("  events                    = {}", def.events.len());
        for (ei, ev) in def.events.iter().enumerate() {
            println!("  event[{ei}]: delay={:.5}..{:.5}  duration={:.5}..{:.5}  die_when_ends={}",
                ev.delay_bounds.lower, ev.delay_bounds.upper,
                ev.duration_bounds.lower, ev.duration_bounds.upper,
                ev.flags.contains(EffectEventFlags::ParticlesDieWhenEffectEnds));
        }
    }
    Ok(())
}
