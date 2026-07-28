//! Why does `frame_event_list`'s CookedAssetsReferencedByTag list sounds/effects
//! that the reference walker doesn't find? Dump the tag's structure.
use blam_tags::api::TagStruct;
use blam_tags::fields::{TagFieldData, TagFieldType};
use blam_tags::iostore::IoStoreArchive;
use blam_tags::TagFile;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
fn walk(s: &TagStruct, depth: usize, out: &mut Vec<String>) {
    for f in s.fields_all() {
        match f.field_type() {
            TagFieldType::TagReference => {
                if let Some(TagFieldData::TagReference(r)) = f.value() {
                    let d = r.group_tag_and_name.map(|(g,p)| format!("{} {p}",
                        String::from_utf8_lossy(&g.to_be_bytes()).to_string()))
                        .unwrap_or_else(|| "<null>".into());
                    out.push(format!("{}{} = {d}", "  ".repeat(depth), f.name()));
                }
            }
            TagFieldType::Struct => if let Some(x)=f.as_struct() { walk(&x, depth+1, out) },
            TagFieldType::Block => if let Some(b)=f.as_block() {
                out.push(format!("{}[block {} x{}]", "  ".repeat(depth), f.name(), b.len()));
                for el in b.iter() { walk(&el, depth+1, out) }
            },
            TagFieldType::Array => if let Some(a)=f.as_array() { for el in a.iter() { walk(&el, depth+1, out) } },
            _ => {}
        }
    }
}
fn main() {
    let want = std::env::args().nth(1).unwrap_or_else(|| "brute-frame_event_list.ubulk".into()).to_lowercase();
    let mut u: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    for utoc in &u {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        let Some(rel) = a.entries().iter().find(|e| e.path.to_lowercase().replace('\\',"/").ends_with(&want)).map(|e| e.path.clone()) else { continue };
        let blob = a.read(&rel).unwrap();
        let tag = TagFile::read_from_bytes(&blob).unwrap();
        println!("{rel}  ({} bytes)", blob.len());
        println!("root struct: {} fields", tag.root().fields_all().count());
        for f in tag.root().fields_all() {
            let extra = match f.field_type() {
                TagFieldType::Block => f.as_block().map(|b| format!(" x{}", b.len())).unwrap_or_default(),
                _ => String::new(),
            };
            println!("   {:?} {}{}", f.field_type(), f.name(), extra);
        }
        let mut out = Vec::new();
        walk(&tag.root(), 0, &mut out);
        println!("\nwalker found {} lines:", out.len());
        for l in out.iter().take(60) { println!("{l}"); }
        return;
    }
    eprintln!("not found");
}
