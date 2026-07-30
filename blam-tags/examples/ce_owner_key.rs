//! What tail family does a class resolve to?
//! Run: `ce_owner_key <class-key>...`
use blam_tags::iostore::object::unversioned::tail_owners;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::world::World;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let mut usmap = Usmap::meteorite().expect("bundled usmap");
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);
    let mut world = World::open(PAKS, usmap).expect("mount Paks");
    let (n, _) = world.register_generated_classes();
    println!("registered {n}");
    let usmap = world.usmap();
    for c in std::env::args().skip(1) {
        let mut chain = Vec::new();
        let mut cur = c.clone();
        for _ in 0..32 {
            chain.push(cur.clone());
            match usmap.get(&cur).and_then(|s| s.super_name.clone()) {
                Some(s) => cur = s,
                None => break,
            }
        }
        println!("\n{c}\n  chain:  {}\n  owners: {:?}", chain.join(" <- "), tail_owners(&c, usmap));
    }
}
