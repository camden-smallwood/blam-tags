//! Dump every non-null tag_reference in a `.ubulk` tag: (group, blam path).
//!   cargo run -p blam-tags --features iostore --example dump_tag_refs -- <file.ubulk> ...

use blam_tags::file::TagFile;
use blam_tags::{TagField, TagFieldData, TagStruct, format_group_tag};

fn collect(st: &TagStruct<'_>, out: &mut Vec<(u32, String)>) {
    for f in st.fields() {
        collect_field(&f, out);
    }
}

fn collect_field(f: &TagField<'_>, out: &mut Vec<(u32, String)>) {
    if let Some(nested) = f.as_struct() {
        collect(&nested, out);
        return;
    }
    if let Some(block) = f.as_block() {
        for elem in block.iter() {
            collect(&elem, out);
        }
        return;
    }
    if let Some(arr) = f.as_array() {
        for elem in arr.iter() {
            collect(&elem, out);
        }
        return;
    }
    if let Some(TagFieldData::TagReference(r)) = f.value() {
        if let Some((g, p)) = r.group_tag_and_name {
            out.push((g, p));
        }
    }
}

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let tag = match TagFile::read_from_bytes(&bytes) {
            Ok(t) => t,
            Err(e) => {
                println!("{path}: parse error: {e}");
                continue;
            }
        };
        let mut refs = Vec::new();
        collect(&tag.root(), &mut refs);
        println!("\n{} — {} references:", path.rsplit('/').next().unwrap(), refs.len());
        for (g, p) in &refs {
            println!("  [{}] {}", format_group_tag(*g), p);
        }
    }
}
