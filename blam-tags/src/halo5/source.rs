use super::{Definitions, Document};
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

/// Detect by content; MCC and Halo 5 share tag extensions such as .biped.
pub fn is_halo5_file(path: &Path) -> bool {
    let mut header = [0u8; 8];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_ok()
        && &header[..4] == b"ucsh"
        && matches!(
            u32::from_le_bytes(header[4..8].try_into().unwrap()),
            23 | 27
        )
}

/// Bounded discovery keeps Load Folder responsive in large MCC kits. The
/// explicit Halo 5 opener also handles tags beyond this discovery sample.
pub fn is_halo5_folder(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    let dump = path.join("tag_dump");
    let root = if dump.is_dir() { dump.as_path() } else { path };
    walkdir::WalkDir::new(root)
        .max_depth(8)
        .into_iter()
        .take(256)
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .take(32)
        .any(|entry| is_halo5_file(entry.path()))
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub group_name: String,
    pub group: String,
    pub path: PathBuf,
}
#[derive(Clone)]
pub struct Source {
    pub root: PathBuf,
    pub entries: Vec<Entry>,
    pub definitions: Definitions,
    pub skipped: Vec<String>,
}
impl Source {
    /// Browse extracted tags; archives are never opened or scanned.
    pub fn open(path: &Path, definitions: Definitions) -> Result<Self> {
        ensure!(
            path.is_dir() || is_halo5_file(path),
            "choose an extracted Halo 5 tag file or a folder of tags"
        );
        let mut source = Self {
            root: path.to_owned(),
            entries: Vec::new(),
            definitions,
            skipped: Vec::new(),
        };
        if path.is_dir() {
            // A complete dump can include 1,000+ archives alongside an already
            // extracted tag_dump. Prefer that tree to listing duplicates.
            let dump = path.join("tag_dump");
            let root = if dump.is_dir() { dump.as_path() } else { path };
            for file in walkdir::WalkDir::new(root) {
                let file = match file {
                    Ok(f) => f,
                    Err(e) => {
                        source.skipped.push(e.to_string());
                        continue;
                    }
                };
                if !file.file_type().is_file() {
                    continue;
                }
                let p = file.path();
                if p.extension()
                    .is_some_and(|s| s.eq_ignore_ascii_case("module"))
                {
                    continue;
                }
                if let Err(e) = source.add_loose(p, root) {
                    source.skipped.push(format!("{}: {e:#}", p.display()));
                }
            }
        } else {
            source.add_loose(path, path.parent().unwrap_or(Path::new(".")))?;
        }
        ensure!(
            !source.entries.is_empty(),
            "no Halo 5 tags found at {}{}",
            path.display(),
            source
                .skipped
                .first()
                .map_or(String::new(), |s| format!(": {s}"))
        );
        source.entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(source)
    }
    fn add_loose(&mut self, path: &Path, root: &Path) -> Result<()> {
        let Some(extension) = path.extension().and_then(|s| s.to_str()) else {
            return Ok(());
        };
        // Resource dumps can outnumber tags by an order of magnitude. They
        // have no editable layout and need no per-file header read.
        if self.definitions.group_for_name(extension).is_none() {
            return Ok(());
        }
        // Index by known group extension without opening hundreds of thousands
        // of files. Document::parse validates the ucsh header on selection.
        let group_name = path
            .extension()
            .and_then(|s| s.to_str())
            .context("extracted tag needs its original group-name extension")?
            .to_owned();
        let group = self
            .definitions
            .group_for_name(&group_name)
            .unwrap_or_else(|| "????".to_owned());
        self.entries.push(Entry {
            name: path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/"),
            group_name,
            group,
            path: path.to_owned(),
        });
        Ok(())
    }
    pub fn read(&self, index: usize) -> Result<Document> {
        let entry = self
            .entries
            .get(index)
            .context("source entry out of range")?;
        Document::open(&entry.path, self.definitions.schema(&entry.group_name)?)
    }
    /// Resolve a metadata reference against extracted files. Ancestors allow
    /// a single opened tag or a subfolder to resolve paths relative to the
    /// dump's tag root without scanning the entire dump first.
    pub fn with_loose_reference(&self, group: &str, name: &str) -> Result<(Self, usize)> {
        let extension = self
            .definitions
            .names()
            .find(|name| {
                self.definitions
                    .group_for_name(name)
                    .is_some_and(|g| format!("{g:<4}") == format!("{group:<4}"))
            })
            .context("reference group has no tag definition")?;
        let name = name.replace('\\', "/");
        let name = name.trim_end_matches('/');
        ensure!(
            !name.is_empty()
                && !name.starts_with('/')
                && !name.contains(':')
                && name
                    .split('/')
                    .all(|part| part != ".." && part != "." && !part.is_empty()),
            "invalid reference path"
        );
        let suffix = format!(".{extension}");
        let relative = if name.to_ascii_lowercase().ends_with(&suffix) {
            name.to_owned()
        } else {
            format!("{name}{suffix}")
        };
        if let Some(index) = self.entries.iter().position(|e| {
            e.group == group && e.name.replace('\\', "/").eq_ignore_ascii_case(&relative)
        }) {
            return Ok((self.clone(), index));
        }
        let base = if self.root.is_dir() {
            self.root.as_path()
        } else {
            self.root.parent().context("tag has no parent folder")?
        };
        let path = base
            .ancestors()
            .flat_map(|base| [base.join("tag_dump").join(&relative), base.join(&relative)])
            .find(|path| path.is_file())
            .context("referenced extracted tag was not found")?;
        ensure!(is_halo5_file(&path), "referenced file is not a Halo 5 tag");
        if let Some(index) = self.entries.iter().position(|e| e.path == path) {
            return Ok((self.clone(), index));
        }
        let mut next = self.clone();
        let index = next.entries.len();
        next.entries.push(Entry {
            name: relative,
            group_name: extension.to_owned(),
            group: group.to_owned(),
            path,
        });
        Ok((next, index))
    }
}
