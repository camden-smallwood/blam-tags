//! Prototype BLUEPRINT-DRIVEN CE mesh resolution: find the character's
//! BP_*BipedActor (via the DA_MeshSynchronization link), read its authoritative
//! mesh soft-ref list, and match against the hlmt variants' (region,perm).
//! Run: cargo run -p blam-tags --features iostore --example ce_preview_debug -- [model-key]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::sync::Arc;
use blam_tags::file::TagFile;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn norm(p: &str) -> String { p.to_ascii_lowercase().replace('\\', "/") }

fn main() {
    let key = norm(&std::env::args().nth(1).unwrap_or_else(|| "objects/characters/marine/marine-model".into()));
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    utocs.sort();
    let ar: Vec<Arc<IoStoreArchive>> = utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let read = |suf: &str| { let s = suf.to_ascii_lowercase();
        ar.iter().find_map(|a| a.entries().iter().find(|e| norm(&e.path).ends_with(&s)).and_then(|e| a.read(&e.path).ok())) };

    // hlmt variants (region -> perm)
    let hlmt = TagFile::read_from_bytes(&read(&format!("{key}.ubulk")).expect("model")).unwrap();
    let mut needed: BTreeSet<(String, String)> = BTreeSet::new();
    if let Some(vb) = hlmt.root().field_path("variants").and_then(|f| f.as_block()) {
        for i in 0..vb.len() { let Some(v)=vb.element(i) else{continue};
            if let Some(rb)=v.field("regions").and_then(|f|f.as_block()) {
                for j in 0..rb.len() { let Some(r)=rb.element(j) else{continue};
                    let rn=r.read_string_id("region name").unwrap_or_default();
                    let pn=r.field("permutations").and_then(|f|f.as_block()).and_then(|pb|pb.element(0))
                        .and_then(|p|p.read_string_id("permutation name")).unwrap_or_default();
                    if !rn.is_empty()&&!pn.is_empty(){needed.insert((rn.to_ascii_lowercase(),pn.to_ascii_lowercase()));}
    }}}}

    // DA_MeshSynchronization importing this model -> DA basename.
    let mut da_base = None;
    'scan: for a in &ar { for e in a.entries() { let n = norm(&e.path);
        if !n.ends_with("meshsynchronization.uasset") { continue; }
        let Ok(b)=a.read(&e.path) else{continue};
        let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None) else{continue};
        if h.imported_package_names.iter().any(|p| norm(p).ends_with(&key)) {
            da_base = n.rsplit('/').next().map(|s| s.strip_suffix(".uasset").unwrap_or(s).to_string()); break 'scan;
    }}}
    let da_base = da_base.expect("no DA");
    println!("DA: {da_base}");

    // BP_*BipedActor importing that DA -> pick the one with the most mesh soft-refs.
    let mut best: Option<(String, usize, Vec<String>)> = None;
    for a in &ar { for e in a.entries() { let n = norm(&e.path);
        if !n.ends_with("bipedactor.uasset") { continue; }
        let Ok(b)=a.read(&e.path) else{continue};
        let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None) else{continue};
        if !h.imported_package_names.iter().any(|p| norm(p).ends_with(&da_base)) { continue; }
        // mesh soft-refs = name-map entries that look like /Mesh/ SK_ or SM_ paths
        let meshes: Vec<String> = h.name_map.copy_raw_names().into_iter()
            .filter(|s| { let l=norm(s); l.contains("/mesh/")&&(l.contains("/sk_")||l.contains("/sm_")) }).collect();
        let bp = n.rsplit('/').next().unwrap_or("").to_string();
        if best.as_ref().map_or(true, |(_,c,_)| meshes.len()>*c) { best=Some((bp, meshes.len(), meshes)); }
    }}
    let Some((bp, _, meshes)) = best else { println!("no BP found"); return; };
    println!("blueprint: {bp}  ({} mesh soft-refs)", meshes.len());
    // basename(no ext) -> full path
    let mut sk: BTreeMap<String,String> = BTreeMap::new();
    for m in &meshes { let base = norm(m).rsplit('/').next().unwrap_or("").to_string(); sk.insert(base, m.clone()); }
    println!("mesh stems: {:?}", sk.keys().take(60).collect::<Vec<_>>());

    // Match each needed (region, perm) to a BP mesh (exact sk_<char>_<perm>, else ends_with _<perm>).
    println!("\nvariant (region,perm) coverage from the blueprint list:");
    let mut got=0;
    for (region, perm) in &needed {
        let want_end = format!("_{perm}");
        let m = sk.keys().filter(|s| s.ends_with(perm) || s.ends_with(&want_end)).min_by_key(|s| s.len()).cloned();
        if m.is_some(){got+=1;}
        println!("  {region:<8}/{perm:<16} -> {}", m.as_deref().unwrap_or("(none)"));
    }
    println!("\nresolved {got}/{} from blueprint list", needed.len());
    let matched: BTreeSet<String> = needed.iter().filter_map(|(_,p)| sk.keys().find(|s| s.ends_with(p.as_str())).cloned()).collect();
    let unused: Vec<&String> = sk.keys().filter(|s| !matched.contains(*s)).collect();
    println!("BP meshes NOT tied to a variant perm (base/always-on?): {:?}", unused);
}
