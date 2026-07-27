//! Top-down recursive dependency search across a Campaign Evolved container
//! set: start at one tag, follow every `tag_reference` in its data tree, and
//! recurse — reporting the *structural* path to each tag reached, not a name
//! match.
//!
//! Run:
//!   cargo run --release -p blam-tags --features iostore --example ce_dep_search -- \
//!     <root-tag-substr> [--want <group-fourcc>] [--depth N]
//!
//! e.g. `-- battle_rifle-weapon --want snd! --depth 6`

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use blam_tags::file::TagFile;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::paths::group_tag_to_extension;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn norm(p: &str) -> String {
    p.replace('\u{0}', "").trim().replace('\\', "/").to_ascii_lowercase()
}

fn fourcc(g: u32) -> String {
    String::from_utf8_lossy(&g.to_be_bytes()).replace('\u{0}', " ")
}

/// Every `.ubulk` tag payload in the mounted containers, keyed by the logical
/// tag identity a reference carries: `<halo-relative path>-<group extension>`.
struct TagIndex {
    archives: Vec<Arc<IoStoreArchive>>,
    by_key: HashMap<String, (usize, String)>,
}

impl TagIndex {
    fn build() -> anyhow::Result<Self> {
        let mut utocs: Vec<_> = std::fs::read_dir(PAKS)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
            .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
            .collect();
        utocs.sort();

        let mut archives = Vec::new();
        let mut by_key = HashMap::new();
        for utoc in &utocs {
            let Ok(a) = IoStoreArchive::open(utoc) else { continue };
            let a = Arc::new(a);
            let ai = archives.len();
            for e in a.entries() {
                let n = norm(&e.path);
                let Some(stem) = n.strip_suffix(".ubulk") else { continue };
                // Cooked path -> tag identity: strip the `<root>/content/tags/`
                // prefix; what remains is exactly `<tag path>-<extension>`.
                let Some((_, rest)) = stem.split_once("/content/tags/") else { continue };
                // Later packs win, matching the mount's layering.
                by_key.insert(rest.to_string(), (ai, e.path.clone()));
            }
            archives.push(a);
        }
        Ok(Self { archives, by_key })
    }

    /// Resolve a `(group, path)` reference exactly as the engine would.
    fn read(&self, group: u32, path: &str) -> Option<TagFile> {
        let key = self.key(group, path)?;
        let (ai, rel) = self.by_key.get(&key)?;
        let bytes = self.archives[*ai].read(rel).ok()?;
        TagFile::read_from_bytes(&bytes).ok()
    }

    fn key(&self, group: u32, path: &str) -> Option<String> {
        Some(format!("{}-{}", norm(path), group_tag_to_extension(group)?))
    }

    fn find(&self, substr: &str) -> Vec<&String> {
        let s = substr.to_ascii_lowercase();
        let mut v: Vec<&String> = self.by_key.keys().filter(|k| k.contains(&s)).collect();
        v.sort();
        v
    }
}

/// One outgoing reference plus the field path it was found at.
struct Ref {
    group: u32,
    path: String,
    at: String,
}

/// Walk a tag's data tree collecting every non-null reference, recording the
/// field-name breadcrumb so the caller can see *where* a dependency hangs
/// (`barrels[0].firing effect`) rather than just that it exists.
fn collect(st: &blam_tags::TagStruct<'_>, prefix: &str, out: &mut Vec<Ref>) {
    for f in st.fields() {
        let name = f.name();
        let at = if prefix.is_empty() { name.to_string() } else { format!("{prefix}.{name}") };
        if let Some(nested) = f.as_struct() {
            collect(&nested, &at, out);
        } else if let Some(block) = f.as_block() {
            for (i, elem) in block.iter().enumerate() {
                collect(&elem, &format!("{at}[{i}]"), out);
            }
        } else if let Some(arr) = f.as_array() {
            for (i, elem) in arr.iter().enumerate() {
                collect(&elem, &format!("{at}[{i}]"), out);
            }
        } else if let Some(blam_tags::TagFieldData::TagReference(r)) = f.value()
            && let Some((g, p)) = r.group_tag_and_name
            && !p.replace('\u{0}', "").trim().is_empty()
        {
            out.push(Ref { group: g, path: p, at });
        }
    }
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = args.next().expect("usage: <root-tag-substr> [--want <fourcc>] [--depth N]");
    let mut want: Option<String> = None;
    let mut max_depth = 8usize;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--want" => want = args.next(),
            "--depth" => max_depth = args.next().and_then(|d| d.parse().ok()).unwrap_or(8),
            _ => {}
        }
    }

    eprintln!("indexing containers...");
    let idx = TagIndex::build()?;
    eprintln!("indexed {} tag payloads", idx.by_key.len());

    let hits = idx.find(&root);
    let Some(start) = hits.first() else {
        anyhow::bail!("no tag payload matching {root:?}");
    };
    if hits.len() > 1 {
        eprintln!("note: {} matches, using {start}", hits.len());
    }

    // Seed from the start tag's own key: split `<path>-<ext>` back apart by
    // finding the extension our table agrees with.
    let (spath, sext) = start.rsplit_once('-').expect("indexed key always has -<ext>");
    println!("root: {spath}.{sext}\n");

    let (ai, rel) = idx.by_key.get(*start).unwrap();
    let bytes = idx.archives[*ai].read(rel)?;
    let tag = TagFile::read_from_bytes(&bytes).map_err(|e| anyhow::anyhow!("{e:?}"))?;

    // Depth-first walk, tracking the chain so a hit prints its provenance.
    let mut visited: HashSet<String> = HashSet::new();
    let mut matches: Vec<Vec<String>> = Vec::new();
    let mut unresolved: BTreeMap<String, usize> = BTreeMap::new();
    let mut stack: Vec<(TagFile, Vec<String>, usize)> = Vec::new();
    stack.push((tag, vec![format!("{spath}.{sext}")], 0));
    visited.insert((*start).clone());

    let mut visited_count = 0usize;
    while let Some((tag, chain, depth)) = stack.pop() {
        visited_count += 1;
        let mut refs = Vec::new();
        collect(&tag.root(), "", &mut refs);

        for r in refs {
            let ext = group_tag_to_extension(r.group).unwrap_or("?");
            let Some(key) = idx.key(r.group, &r.path) else {
                *unresolved.entry(format!("<unknown group {}>", fourcc(r.group))).or_default() += 1;
                continue;
            };

            let mut next_chain = chain.clone();
            next_chain.push(format!("{}.{}  @ {}", norm(&r.path), ext, r.at));

            if want.as_deref().is_some_and(|w| w == fourcc(r.group).trim_end()) {
                matches.push(next_chain.clone());
            }

            if depth + 1 > max_depth || !visited.insert(key.clone()) {
                continue;
            }
            let Some(child) = idx.read(r.group, &r.path) else {
                *unresolved.entry(format!("{}.{ext}", norm(&r.path))).or_default() += 1;
                continue;
            };
            stack.push((child, next_chain, depth + 1));
        }
    }

    println!("visited {visited_count} tags (depth<={max_depth}), {} unresolved", unresolved.len());

    if let Some(w) = &want {
        println!("\n=== {} chains reaching group '{w}' ===", matches.len());
        // Shortest chains first: the most direct binding is the interesting one.
        matches.sort_by_key(|c| (c.len(), c.join("/")));
        for chain in &matches {
            println!();
            for (i, step) in chain.iter().enumerate() {
                println!("{}{}", "  ".repeat(i), step);
            }
        }
    }
    Ok(())
}
