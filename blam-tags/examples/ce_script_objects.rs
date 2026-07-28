//! Load the global container's script-object table and check it round-trips.
//!
//! Every resolved path is re-hashed and compared against the global index it
//! was read from — a path built with the wrong separator or a mis-walked outer
//! chain will not match, so the verified count is a real correctness gate.
use blam_tags::iostore::script_objects::ScriptObjects;

const GLOBAL: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks/global.utoc";

fn main() {
    let so = ScriptObjects::load(GLOBAL).expect("load script objects");
    let verified = so.verified_count();
    println!("script objects: {}", so.len());
    println!("paths resolved: {}", so.entries().len());
    println!("hash round-trips: {verified}");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let probes: Vec<String> = if args.is_empty() {
        ["/Script/Engine.StaticMeshComponent", "/Script/Engine.SkeletalMesh"]
            .iter().map(|s| s.to_string()).collect()
    } else { args };
    for probe in &probes {
        use blam_tags::iostore::ue_types::FPackageObjectIndex;
        let h = FPackageObjectIndex::create_script_import(probe).raw_index();
        println!("  {probe} -> {:?}", so.resolve(h));
    }
}
