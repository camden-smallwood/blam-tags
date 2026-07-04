//! CLI-level session state.
//!
//! [`CliContext`] is threaded through every command's `run`. For
//! tag-bound commands the driver loads the target tag into
//! [`CliContext::loaded`] before dispatching; commands mutate through
//! the facade and set [`LoadedTag::dirty`]; the driver or the command
//! decides when to persist.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use blam_tags::classic::{read_classic_tag_file, ClassicHeader};
use blam_tags::layout::TagLayout;
use blam_tags::monolithic::MonolithicCache;
use blam_tags::paths::{derive_tags_root, resolve_tag_path};
use blam_tags::TagFile;

use crate::tag_index::TagIndex;

/// Per-invocation session state — the loaded tag (if any), the
/// REPL navigation prefix, and the loaded `_meta.json` tag index.
/// Threaded through every command's `run`.
pub struct CliContext {
    /// The tag the next command will operate on. `None` when no tag
    /// is loaded (one-shot mode pre-load, or REPL `close` state).
    pub loaded: Option<LoadedTag>,
    /// REPL navigation prefix — each segment is a path fragment the
    /// user entered via `edit-block`. Empty in one-shot mode and at
    /// the Tag context in the REPL. Commands concatenate this with
    /// the user-supplied path to produce the actual field-path.
    pub nav: Vec<String>,
    /// Game identifier (e.g. `"haloreach_mcc"`). Set once at startup
    /// from the global `--game` flag and used to scope schema lookups
    /// and the [`TagIndex`]. `None` when invoked without `--game` —
    /// in that mode read commands work using each tag's embedded
    /// `blay` schema and tag-reference rendering falls back to raw
    /// 4-byte group tags; write/create commands that need an external
    /// JSON schema (`new`, `add-want`, `add-info`, `add-assd`,
    /// `rebuild-dependencies`) error out asking for `--game`.
    pub game: Option<String>,
    /// group_tag ↔ group-name index loaded from
    /// `definitions/<game>/_meta.json`. Empty default when no
    /// `--game` is provided — render paths show the raw group tag in
    /// place of the friendly group name.
    pub tag_index: TagIndex,
    /// Opened monolithic tag cache, when the global `--cache` flag
    /// is set. Reads of tag-file arguments resolve through this
    /// instead of opening files on disk.
    pub cache: Option<MonolithicCache>,
}

/// A parsed tag plus the path it came from, with a dirty flag for
/// REPL save-prompt logic.
pub struct LoadedTag {
    /// Path the tag was read from. Used as the default save target.
    pub path: PathBuf,
    /// Parsed tag tree.
    pub tag: TagFile,
    /// `true` once a command has mutated the tag without persisting.
    pub dirty: bool,
}

impl CliContext {
    /// Build a context for the given game. When `game` is `Some`,
    /// eagerly loads `definitions/<game>/_meta.json` — errors if that
    /// file is missing or malformed. When `game` is `None`,
    /// initializes with an empty [`TagIndex`]; read commands keep
    /// working (using each tag's embedded `blay`) while write
    /// commands that need external schemas surface a typed error at
    /// dispatch time.
    pub fn new(game: Option<&str>, cache: Option<&str>) -> Result<Self> {
        let (game, tag_index) = match game {
            Some(g) => (Some(g.to_owned()), TagIndex::load(Path::new("definitions"), g)?),
            None => (None, TagIndex::default()),
        };
        let cache = match cache {
            Some(p) => Some(
                MonolithicCache::open(p)
                    .map_err(|e| anyhow::anyhow!("failed to open cache {p}: {e}"))?,
            ),
            None => None,
        };
        Ok(Self { loaded: None, nav: Vec::new(), game, tag_index, cache })
    }

    /// Return the game identifier, or an error referencing the
    /// requested command. Used by write/create commands that need an
    /// external JSON schema and can't fall back to embedded layouts.
    pub fn require_game(&self, cmd: &str) -> Result<&str> {
        self.game.as_deref().with_context(|| {
            format!("`{cmd}` needs a `--game` (e.g. `--game halo3_mcc`) to locate its schema")
        })
    }

    /// Load `path` into [`Self::loaded`] and reset [`Self::nav`] to
    /// the root. Replaces any currently-loaded tag without prompting
    /// — dirty-state handling is the caller's job.
    ///
    /// When [`Self::cache`] is set, the path is interpreted as a
    /// cache-relative tag path with extension (e.g.
    /// `objects/elite/elite.biped`). Otherwise it's a filesystem
    /// path.
    pub fn load(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let (tag, source_path) = if let Some(cache) = &self.cache {
            let (group_tag, name) = self.split_cache_tag_path(path)?;
            let tag = cache
                .read_tag_by_name(group_tag, &name)
                .map_err(|e| anyhow::anyhow!("failed to load `{}` from cache: {e}", path.display()))?;
            (tag, path.to_path_buf())
        } else {
            let tag = self.read_tag_file(path)?;
            (tag, path.to_path_buf())
        };
        self.loaded = Some(LoadedTag { path: source_path, tag, dirty: false });
        self.nav.clear();
        Ok(())
    }

    /// Read a tag from the filesystem, routing classic (Halo CE / H2)
    /// tags through the JSON-def synthesized layout and everything else
    /// through the MCC reader.
    ///
    /// Classic tags have no embedded `blay`; they're recognized by the
    /// engine word at offset 60 ([`ClassicHeader::parse`]). The group is
    /// taken from the **header's group tag** (offset 36), resolved to a
    /// group name via the `--game` [`TagIndex`], and its
    /// `definitions/<game>/<name>.json` def is synthesized into a layout.
    pub fn read_tag_file(&self, path: &Path) -> Result<TagFile> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        if let Some((header, _engine)) = ClassicHeader::parse(&bytes) {
            let group_tag = u32::from_be_bytes(header.group_tag);
            let game = self.game.as_deref().context(
                "classic Halo CE / Halo 2 tags carry no embedded layout — pass a `--game` \
                 (e.g. `--game haloce_mcc`) so the JSON definition can be located",
            )?;
            let name = self.tag_index.name_for(group_tag).with_context(|| {
                format!(
                    "no group definition for group tag {:?} in definitions/{game}/",
                    String::from_utf8_lossy(&header.group_tag).trim_end()
                )
            })?;
            let def_path = Path::new("definitions").join(game).join(format!("{name}.json"));
            let layout = TagLayout::from_json(&def_path).map_err(|e| {
                anyhow::anyhow!("failed to load classic layout {}: {e}", def_path.display())
            })?;
            return read_classic_tag_file(&bytes, layout).map_err(|e| {
                anyhow::anyhow!("failed to decode classic tag {}: {e}", path.display())
            });
        }

        TagFile::read(path).map_err(|e| anyhow::anyhow!("failed to load tag file: {e}"))
    }

    /// Split a `--cache`-mode tag path like
    /// `objects/elite/elite.biped` into `(group_tag, cache_relative_name)`.
    /// Tries the [`TagIndex`] first (matching `biped` → `bipd`) so
    /// users can write the friendly extension when `--game` is set;
    /// falls back to a literal 4-char ASCII group tag otherwise.
    fn split_cache_tag_path(&self, path: &Path) -> Result<(u32, String)> {
        let s = path
            .to_str()
            .with_context(|| format!("non-UTF-8 cache path {}", path.display()))?;
        let (name, ext) = s
            .rsplit_once('.')
            .with_context(|| format!("cache path `{s}` missing group extension"))?;
        let group_tag = self.tag_index.group_tag_for(ext).or_else(|| blam_tags::parse_group_tag(ext))
            .with_context(|| {
                format!(
                    "cache path `{s}` has unrecognized group extension `.{ext}` \
                     (pass `--game` to use the friendly name, or use the 4-char tag like `.bipd`)",
                )
            })?;
        let name = name.replace('/', "\\");
        Ok((group_tag, name))
    }

    /// Resolve a tag reference (a relative tag-path like
    /// `objects\elite\elite` plus a group extension like
    /// `"render_model"`) to a parsed [`TagFile`]. Dispatches on
    /// cache-vs-filesystem mode:
    ///
    /// - **`--cache` set:** looks the tag up by `(group_tag, name)`
    ///   in the monolithic cache. The group tag is resolved from
    ///   `group_ext` via [`TagIndex`] (friendly name) first, then
    ///   [`blam_tags::parse_group_tag`] (4-char ASCII) as a fallback.
    /// - **filesystem mode:** derives a `tags/` root from the
    ///   currently-loaded tag's path and reads
    ///   `<tags_root>/<rel>.<ext>`.
    ///
    /// Used by `extract-geometry` / `extract-animation` / etc. to
    /// resolve child references (`hlmt` → `mode`/`coll`/`phmo`,
    /// `mode` → `jmad`/`bitm`, etc.).
    pub fn load_referenced_tag(&self, rel_path: &str, group_ext: &str) -> Result<TagFile> {
        if let Some(cache) = &self.cache {
            let group_tag = self
                .tag_index
                .group_tag_for(group_ext)
                .or_else(|| blam_tags::parse_group_tag(group_ext))
                .with_context(|| {
                    format!(
                        "unknown group `{group_ext}` (pass `--game` to use the friendly name)",
                    )
                })?;
            return cache.read_tag_by_name(group_tag, rel_path).map_err(|e| {
                anyhow::anyhow!("failed to load {group_ext}:{rel_path} from cache: {e}")
            });
        }

        let loaded = self.loaded("load_referenced_tag")?;
        let tags_root = derive_tags_root(&loaded.path).context(
            "failed to derive tags root from input path — input must live under a `tags/` directory \
             (or pass `--cache` to read from a monolithic cache)",
        )?;
        let path = resolve_tag_path(&tags_root, rel_path, group_ext);
        // Route through `read_tag_file` (not the MCC-only `TagFile::read`)
        // so referenced classic CE/H2 tags decode via the classic path.
        self.read_tag_file(&path)
    }

    /// True when this context resolves tag references through a
    /// monolithic cache rather than the filesystem.
    pub fn is_cache_mode(&self) -> bool {
        self.cache.is_some()
    }

    /// Immutable borrow of the loaded tag. Context is the command
    /// name for the error message when nothing is loaded.
    pub fn loaded(&self, cmd: &str) -> Result<&LoadedTag> {
        self.loaded
            .as_ref()
            .with_context(|| format!("`{cmd}` needs a loaded tag"))
    }

    /// Mutable borrow of the loaded tag.
    pub fn loaded_mut(&mut self, cmd: &str) -> Result<&mut LoadedTag> {
        self.loaded
            .as_mut()
            .with_context(|| format!("`{cmd}` needs a loaded tag"))
    }

    /// Resolve a user-supplied path against the current navigation
    /// prefix. Paths starting with `/` are absolute (nav stripped);
    /// everything else is concatenated onto the nav prefix.
    pub fn resolve_path(&self, user_path: &str) -> String {
        if let Some(rest) = user_path.strip_prefix('/') {
            rest.to_string()
        } else if self.nav.is_empty() {
            user_path.to_string()
        } else if user_path.is_empty() {
            self.nav.join("/")
        } else {
            format!("{}/{}", self.nav.join("/"), user_path)
        }
    }
}

impl LoadedTag {
    /// Write the tag to `dest` (or back to [`Self::path`] if `dest` is
    /// `None`). Clears the dirty flag on success.
    pub fn save(&mut self, dest: Option<&Path>) -> Result<PathBuf> {
        let target = dest.map(Path::to_path_buf).unwrap_or_else(|| self.path.clone());
        self.tag
            .write(&target)
            .map_err(|e| anyhow::anyhow!("failed to save tag file: {e}"))?;
        self.dirty = false;
        Ok(target)
    }

    /// Save and report where it went. Convenience for mutating
    /// commands — returns `(target_path, was_redirected)` so callers
    /// can print a "saved to <path>" line only when output differs
    /// from the source.
    pub fn commit(&mut self, dest: Option<&Path>) -> Result<Commit> {
        let source = self.path.clone();
        let target = self.save(dest)?;
        let redirected = target != source;
        Ok(Commit { target, redirected })
    }
}

/// Result of [`LoadedTag::commit`] — the path written to and a flag
/// indicating whether it differs from the source path. Lets mutating
/// commands print a "saved to …" line only when the user redirected
/// output via `--output`.
pub struct Commit {
    /// Path the tag was written to.
    pub target: PathBuf,
    /// `true` if `target` differs from the source path (i.e. the
    /// user passed `--output`).
    pub redirected: bool,
}


/// Adapts a [`CliContext`] into a [`blam_tags::extract::TagResolver`] so
/// the shared extraction orchestration (`blam_tags::extract`) can resolve
/// child tag references through the CLI's filesystem-or-cache loader —
/// which, unlike a bare `TagFile::read`, also handles classic (Halo CE /
/// Halo 2) tags and monolithic-cache mode.
pub struct CtxResolver<'a> {
    /// The context whose loader resolves references.
    pub ctx: &'a CliContext,
}

impl blam_tags::extract::TagResolver for CtxResolver<'_> {
    fn resolve(
        &self,
        reference: &str,
        group_ext: &str,
        _group_tag: u32,
    ) -> std::result::Result<TagFile, blam_tags::extract::ExtractError> {
        self.ctx
            .load_referenced_tag(reference, group_ext)
            .map_err(|e| blam_tags::extract::ExtractError::resolve(e.to_string()))
    }
}
