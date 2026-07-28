//! Dump the usmap schema for every `Blam*TagDataAsset` class: its inheritance
//! chain and the full set of properties (own + inherited) with types. This is
//! the complete surface a tag `.uasset` wrapper can carry.
//!
//! Run: cargo run --release --features iostore --example ce_tagclass_schema [filter]

use blam_tags::iostore::usmap::Usmap;

const USMAP: &str =
    "/Users/camden/Downloads/5.5.4-1097863+++Meteorite+Rel-i343-Meteorite-2606-CU2-Meteorite.usmap";

fn main() {
    let filter = std::env::args().nth(1).unwrap_or_else(|| "TagDataAsset".into());
    let usmap = Usmap::parse(&std::fs::read(USMAP).expect("usmap")).expect("parse usmap");

    let mut names: Vec<String> = usmap.structs.iter().map(|s| s.name.clone()).filter(|k| k.contains(&filter)).collect();
    names.sort();
    println!("{} classes matching {filter:?}\n", names.len());

    for n in names {
        let mut chain = Vec::new();
        let mut cur = Some(n.clone());
        while let Some(c) = cur {
            let Some(s) = usmap.get(&c) else {
                chain.push(format!("{c} <not in usmap>"));
                break;
            };
            let props: Vec<String> = s
                .properties
                .iter()
                .map(|p| format!("{}: {:?}", p.name, p.ty))
                .collect();
            chain.push(format!("{c} [{}]", props.join(", ")));
            cur = s.super_name.clone();
        }
        println!("{n}");
        for (i, c) in chain.iter().enumerate() {
            println!("    {}{}", "  ".repeat(i), c);
        }
        println!();
    }
}
