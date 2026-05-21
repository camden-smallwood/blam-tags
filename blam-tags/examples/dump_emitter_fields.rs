use blam_tags::TagFile;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let t = TagFile::read(&path).unwrap();
    let root = t.root();
    println!("ROOT fields ({}):", root.fields().count());
    for f in root.fields().take(15) {
        println!("  type={:?}  name={:?}", f.field_type(), f.name());
    }
    let events = root.field("events").and_then(|f| f.as_block());
    println!("\nevents block: present={}", events.is_some());
    if let Some(ev) = events {
        println!("events.len() = {}", ev.len());
        if let Some(ev0) = ev.element(0) {
            println!("\nevents[0] fields:");
            for f in ev0.fields() {
                println!("  type={:?}  name={:?}", f.field_type(), f.name());
            }
            let ps = ev0.field("particle systems").and_then(|f| f.as_block());
            println!("\nparticle_systems present: {}", ps.is_some());
            if let Some(psb) = ps {
                println!("particle_systems.len() = {}", psb.len());
                if psb.len() > 0 {
                    if let Some(ps0) = psb.element(0) {
                        let em = ps0.field("emitters").and_then(|f| f.as_block());
                        println!("emitters present: {}", em.is_some());
                        if let Some(emb) = em {
                            println!("emitters.len() = {}", emb.len());
                            if let Some(em0) = emb.element(0) {
                                println!("\nemitters[0] fields:");
                                for f in em0.fields() {
                                    println!("  type={:?}  name={:?}", f.field_type(), f.name());
                                }
                                if let Some(pm) = em0.field("particle movement").and_then(|f| f.as_struct()) {
                                    println!("\n  particle movement struct fields:");
                                    for f in pm.fields() {
                                        println!("    type={:?}  name={:?}  value={:?}", f.field_type(), f.name(), f.value());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
