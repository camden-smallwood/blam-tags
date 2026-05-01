//! Count `.render_model` tags whose `instance mesh index >= 0` (i.e. carry
//! modular instance geometry — modular character armor, decorators, etc.).

use std::path::{Path, PathBuf};

use blam_tags::{TagFieldData, TagFile};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dirs: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if dirs.is_empty() {
        return Err("usage: instance_placements_sweep <DIR> [<DIR>...]".into());
    }

    let mut paths = Vec::new();
    for d in &dirs {
        collect_render_models(d, &mut paths);
    }
    paths.sort();
    eprintln!("scanning {} render_model tags", paths.len());

    // First pass: extension distribution across ALL render_models, regardless
    // of instance-mesh-index — answers "do any render_models have a non-.ass
    // source?" so we can rule the inverse in or out.
    let mut all_ext_counts: std::collections::BTreeMap<String, usize> = Default::default();
    let mut all_no_info = 0usize;
    for p in &paths {
        let Ok(tag) = TagFile::read(p) else { continue };
        let mut found = false;
        if let Some(info) = tag.import_info() {
            if let Some(files) = info.field("files").and_then(|f| f.as_block()) {
                for i in 0..files.len() {
                    let Some(elem) = files.element(i) else { continue };
                    let path_str = elem
                        .field("path")
                        .and_then(|f| f.value())
                        .and_then(|v| match v {
                            TagFieldData::String(s) | TagFieldData::LongString(s) => Some(s),
                            _ => None,
                        })
                        .unwrap_or_default();
                    if path_str.is_empty() {
                        continue;
                    }
                    let ext = path_str
                        .rsplit('.')
                        .next()
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    *all_ext_counts.entry(ext).or_default() += 1;
                    found = true;
                }
            }
        }
        if !found {
            all_no_info += 1;
        }
    }
    println!("Source-file extensions across ALL {} render_models:", paths.len());
    let mut all_rows: Vec<_> = all_ext_counts.iter().collect();
    all_rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (ext, n) in all_rows {
        println!("  {:>6}  .{}", n, ext);
    }
    println!("  {:>6}  (no info stream)", all_no_info);
    println!();

    let mut hits: Vec<(PathBuf, i64, usize, Vec<String>)> = Vec::new();
    for p in &paths {
        let Ok(tag) = TagFile::read(p) else { continue };
        let Some(field) = tag.root().field("instance mesh index") else { continue };
        let Some(value) = field.value() else { continue };
        let mesh_idx = match value {
            TagFieldData::LongBlockIndex(v) => v as i64,
            TagFieldData::CustomLongBlockIndex(v) => v as i64,
            TagFieldData::ShortBlockIndex(v) => v as i64,
            TagFieldData::LongInteger(v) => v as i64,
            _ => continue,
        };
        if mesh_idx < 0 {
            continue;
        }
        let placements = tag
            .root()
            .field("instance placements")
            .and_then(|f| f.as_block())
            .map(|b| b.len())
            .unwrap_or(0);

        // Pull source-file basenames from the `info` stream so we can see
        // which artist source the importer baked in (.ass for scenes,
        // .jms for the older path, etc.).
        let mut sources: Vec<String> = Vec::new();
        if let Some(info) = tag.import_info() {
            if let Some(files) = info.field("files").and_then(|f| f.as_block()) {
                for i in 0..files.len() {
                    let Some(elem) = files.element(i) else { continue };
                    let path_str = elem
                        .field("path")
                        .and_then(|f| f.value())
                        .and_then(|v| match v {
                            TagFieldData::String(s) | TagFieldData::LongString(s) => Some(s),
                            _ => None,
                        })
                        .unwrap_or_default();
                    if path_str.is_empty() {
                        continue;
                    }
                    let normalized = path_str.replace('\\', "/");
                    let base = normalized
                        .rsplit('/')
                        .next()
                        .unwrap_or(&normalized)
                        .to_string();
                    sources.push(base);
                }
            }
        }
        hits.push((p.clone(), mesh_idx, placements, sources));
    }

    let total = paths.len();
    let with_instances = hits.len();
    let total_placements: usize = hits.iter().map(|(_, _, n, _)| n).sum();
    println!(
        "{}/{} render_model tags carry `instance mesh index >= 0` ({} placements total, avg {:.1}/tag)",
        with_instances,
        total,
        total_placements,
        if with_instances > 0 {
            total_placements as f64 / with_instances as f64
        } else {
            0.0
        }
    );

    let mut histogram: std::collections::BTreeMap<usize, usize> = Default::default();
    for (_, _, n, _) in &hits {
        *histogram.entry(*n).or_default() += 1;
    }
    println!("\nPlacement-count distribution:");
    println!("  {:>6}  {:>6}", "count", "tags");
    for (count, tags) in &histogram {
        println!("  {:>6}  {:>6}", count, tags);
    }

    // Source-file extension distribution from the `info` stream.
    let mut ext_counts: std::collections::BTreeMap<String, usize> = Default::default();
    let mut tags_with_info = 0usize;
    let mut tags_without_info = 0usize;
    for (_, _, _, sources) in &hits {
        if sources.is_empty() {
            tags_without_info += 1;
            continue;
        }
        tags_with_info += 1;
        for src in sources {
            let ext = src
                .rsplit('.')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            *ext_counts.entry(ext).or_default() += 1;
        }
    }
    println!(
        "\nimport_info coverage: {} tags carry source files, {} have no info stream",
        tags_with_info, tags_without_info
    );
    println!("Source-file extensions across all info entries:");
    let mut ext_rows: Vec<_> = ext_counts.iter().collect();
    ext_rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (ext, n) in ext_rows {
        println!("  {:>4}  .{}", n, ext);
    }

    // Top-2-path-segment cluster (e.g. levels/shared, objects/characters).
    let mut clusters: std::collections::BTreeMap<String, usize> = Default::default();
    for (path, _, _, _) in &hits {
        let s = path.to_string_lossy();
        if let Some(after) = s.find("/tags/") {
            let rest = &s[after + 6..];
            let parts: Vec<&str> = rest.splitn(3, '/').collect();
            if parts.len() >= 2 {
                let key = format!("{}/{}", parts[0], parts[1]);
                *clusters.entry(key).or_default() += 1;
            }
        }
    }
    println!("\nTop-level path clustering of hits:");
    let mut cluster_rows: Vec<_> = clusters.iter().collect();
    cluster_rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (k, n) in cluster_rows {
        println!("  {:>4}  {}", n, k);
    }

    let mut sorted = hits.clone();
    sorted.sort_by_key(|(_, _, n, _)| std::cmp::Reverse(*n));
    println!("\nFull list (sorted by placement count):");
    for (path, mesh_idx, n, sources) in sorted.iter() {
        let display = path
            .to_str()
            .and_then(|s| s.find("/tags/").map(|i| &s[i + 6..]))
            .unwrap_or_else(|| path.to_str().unwrap_or("?"));
        let src = if sources.is_empty() {
            "(no info)".to_string()
        } else {
            sources.join(", ")
        };
        println!("  {:>4} placements  mesh[{:>3}]  {}  ←  {}", n, mesh_idx, display, src);
    }

    Ok(())
}

fn collect_render_models(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_render_models(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("render_model") {
            out.push(p);
        }
    }
}
