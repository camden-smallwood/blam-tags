//! `layout-diff` — *schema*-level comparison: field adds / removes /
//! moves / type changes between two tags, struct by struct, from
//! [`blam_tags::schema_compare::diff_layouts`]. For *value* comparison see
//! [`data-diff`](crate::commands::data_diff). Used when tracking down
//! schema drift between engine versions or debugging a field whose layout
//! changed on disk.

use anyhow::Result;
use blam_tags::schema_compare::{diff_layouts, FieldChangeKind};
use blam_tags::{format_group_tag, TagFile};

pub fn run(file_a: &str, file_b: &str) -> Result<()> {
    let tag_a = TagFile::read(file_a).map_err(|e| anyhow::anyhow!("failed to parse first file: {e}"))?;
    let tag_b = TagFile::read(file_b).map_err(|e| anyhow::anyhow!("failed to parse second file: {e}"))?;

    let name = |file: &str| {
        std::path::Path::new(file)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(file)
            .to_owned()
    };
    println!("Layout diff: {} vs {}", name(file_a), name(file_b));
    println!();

    let (group_a, group_b) = (tag_a.group(), tag_b.group());
    if group_a.tag != group_b.tag {
        println!("  group_tag: {} -> {}", format_group_tag(group_a.tag), format_group_tag(group_b.tag));
    }
    let diff = diff_layouts(&tag_a, &tag_b);
    if let Some((from, to)) = diff.version {
        println!("  group_version: {from} -> {to}");
    }
    for struct_diff in &diff.structs {
        let place = if struct_diff.path.is_empty() { "(root)" } else { struct_diff.path.as_str() };
        if struct_diff.name == struct_diff.current_name {
            println!("  struct {} at {place}:", struct_diff.name);
        } else {
            println!("  struct {} -> {} at {place}:", struct_diff.name, struct_diff.current_name);
        }
        if struct_diff.size != struct_diff.current_size {
            let delta = struct_diff.current_size as isize - struct_diff.size as isize;
            println!("    size: {} -> {} ({delta:+})", struct_diff.size, struct_diff.current_size);
        }
        for change in &struct_diff.fields {
            let at = |offset: Option<u32>| offset.map(|o| format!(" @ {o}")).unwrap_or_default();
            match &change.kind {
                FieldChangeKind::Removed { type_name } => {
                    println!("    - {} : {type_name}{}", change.name, at(change.offset))
                }
                FieldChangeKind::Added { type_name } => {
                    println!("    + {} : {type_name}{}", change.name, at(change.current_offset))
                }
                FieldChangeKind::Retyped { from, to } => println!("    ~ {} : {from} -> {to}", change.name),
                FieldChangeKind::Moved => println!(
                    "    > {} : moved{} ->{}",
                    change.name,
                    at(change.offset),
                    at(change.current_offset)
                ),
                FieldChangeKind::BlockMaximum { from, to } => {
                    println!("    # {} : block max_count {from} -> {to}", change.name)
                }
                FieldChangeKind::ArrayLength { from, to } => {
                    println!("    # {} : array count {from} -> {to}", change.name)
                }
            }
        }
    }
    if diff.is_empty() && group_a.tag == group_b.tag {
        println!("  (same layout)");
    }
    Ok(())
}
