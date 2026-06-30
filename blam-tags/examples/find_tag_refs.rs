//! Reverse tag-reference scan: walk every tag file under a tags root and
//! report which tags contain a tag_reference whose path matches a substring.
//! Usage: cargo run --example find_tag_refs [substring]   (default: ash_02)
use blam_tags::api::TagStruct;
use blam_tags::fields::{TagFieldData, TagFieldType};
use blam_tags::TagFile;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

fn collect_refs(s: &TagStruct, out: &mut Vec<(u32, String)>) {
    for f in s.fields() {
        match f.field_type() {
            TagFieldType::TagReference => {
                if let Some(TagFieldData::TagReference(r)) = f.value() {
                    if let Some(gp) = r.group_tag_and_name {
                        out.push(gp);
                    }
                }
            }
            TagFieldType::Struct => {
                if let Some(sub) = f.as_struct() { collect_refs(&sub, out); }
            }
            TagFieldType::Block => {
                if let Some(b) = f.as_block() { for el in b.iter() { collect_refs(&el, out); } }
            }
            TagFieldType::Array => {
                if let Some(a) = f.as_array() { for el in a.iter() { collect_refs(&el, out); } }
            }
            _ => {}
        }
    }
}

fn walk_dir(dir: &Path, files: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { walk_dir(&p, files); } else { files.push(p); }
        }
    }
}

fn group_str(g: u32) -> String {
    let be = String::from_utf8_lossy(&g.to_be_bytes()).trim().to_string();
    let le = String::from_utf8_lossy(&g.to_le_bytes()).trim().to_string();
    if be.chars().all(|c| c.is_ascii_graphic() || c == ' ') && !be.is_empty() { be } else { le }
}

fn main() {
    let root = "/Users/camden/Halo/halo3_mcc/tags";
    let target = std::env::args().nth(1).unwrap_or_else(|| "ash_02".into()).to_lowercase();
    std::panic::set_hook(Box::new(|_| {})); // silence per-file panic spam

    let mut files = Vec::new();
    walk_dir(Path::new(root), &mut files);
    eprintln!("scanning {} tag files for refs matching '*{target}*' ...", files.len());

    let (mut hits, mut read_fail, mut panics) = (0u32, 0u32, 0u32);
    for path in &files {
        let res = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let tag = TagFile::read(path).ok()?;
            let mut refs = Vec::new();
            collect_refs(&tag.root(), &mut refs);
            Some(refs)
        }));
        let refs = match res {
            Ok(Some(r)) => r,
            Ok(None) => { read_fail += 1; continue; }
            Err(_) => { panics += 1; continue; }
        };
        let matched: Vec<_> = refs.iter()
            .filter(|(_, n)| n.to_lowercase().replace('\\', "/").contains(&target))
            .collect();
        if !matched.is_empty() {
            let rel = path.strip_prefix(root).unwrap_or(path);
            println!("{}", rel.display());
            for (g, n) in matched { println!("    -> [{}] {}", group_str(*g), n); }
            hits += 1;
        }
    }
    eprintln!("\n{hits} referencing tags | {read_fail} unreadable | {panics} panicked (of {})", files.len());
}
