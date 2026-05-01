//! `blam-tag-shell` CLI entry point.
//!
//! Two execution modes share one [`Commands`] dispatch:
//! - **One-shot** — `blam-tag-shell --game <G> <SUBCOMMAND> …` runs a
//!   single command against a tag and exits. Tag-bound subcommands
//!   take a `<FILE>` positional, load it via [`CliContext::load`],
//!   run, and persist if the command mutates.
//! - **REPL** — `blam-tag-shell repl [FILE]` opens a persistent
//!   session against a loaded tag. See [`crate::repl`] for the loop;
//!   it dispatches the same [`Commands`] enum after preprocessing
//!   the input line.
//!
//! Subcommand handlers live under [`crate::commands`]; each command
//! file's `//!` documents its semantics in detail. Shared shell
//! infrastructure lives in [`crate::context`] (session state),
//! [`crate::format`] (value rendering), [`crate::parse`] (string →
//! `TagFieldData`), [`crate::walk`] (tree traversal), and
//! [`crate::paths`] (filesystem path helpers).

use anyhow::Result;
use clap::{Parser, Subcommand};

use context::CliContext;

mod commands;
mod context;
mod format;
mod parse;
mod paths;
mod repl;
mod suggest;
mod tag_index;
mod walk;

#[derive(Parser)]
#[command(name = "blam-tag-shell", about = "Halo tag file inspector and editor")]
struct Cli {
    /// Game whose schemas / `_meta.json` tag index this invocation
    /// operates against. Resolves to `definitions/<GAME>/`. Required
    /// — schemas and group-name resolution can't work without it.
    #[arg(long, short = 'g', global = true)]
    game: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Interactive shell — open a tag and run commands against it
    /// without re-parsing on every invocation
    Repl {
        /// Optional tag to load at startup
        file: Option<String>,
    },

    /// Create a new tag from a group schema. Reads the schema JSON
    /// at `definitions/<game>/<group>.json` (game from the global
    /// `--game` flag) and writes a zero-filled tag to
    /// `<group>.<group>` (or `--output`).
    New {
        /// Group name (matches the schema filename, e.g. `biped`)
        group: String,
        /// Output path (default: `<group>.<group>` in cwd)
        #[arg(long)]
        output: Option<String>,
    },

    /// Show field tree
    Inspect {
        /// Path to a tag file
        file: String,
        /// Field path to start from
        path: Option<String>,
        /// Show all fields including hidden
        #[arg(long)]
        all: bool,
        /// Recursively expand everything — including block elements.
        /// Default (flat): show the target's fields and one-step descents
        /// (struct/array/resource), but stop at blocks (display count only).
        /// Drill into a specific block element with `<path>[<index>]`.
        #[arg(long)]
        full: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Only show fields whose name contains any of these comma-separated substrings
        #[arg(long, value_delimiter = ',')]
        filter: Vec<String>,
        /// Skip fields whose name contains any of these comma-separated substrings
        #[arg(long = "filter-not", value_delimiter = ',')]
        filter_not: Vec<String>,
        /// Only show leaf fields whose rendered value contains this substring
        #[arg(long = "filter-value")]
        filter_value: Option<String>,
    },

    /// Read a field value
    Get {
        /// Path to a tag file
        file: String,
        /// Field path (e.g. "jump velocity" or "unit/seats\[0\]/flags")
        path: String,
        /// Output raw value only (no label)
        #[arg(long)]
        raw: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Output numeric values in hex
        #[arg(long)]
        hex: bool,
    },

    /// Write a field value
    Set {
        /// Path to a tag file
        file: String,
        /// Field path
        path: String,
        /// Value to set
        value: String,
        /// Write to a different file
        #[arg(long)]
        output: Option<String>,
        /// Preview changes without writing
        #[arg(long)]
        dry_run: bool,
    },

    /// Get or set flag bits
    Flag {
        /// Path to a tag file
        file: String,
        /// Field path to a flags field
        path: String,
        /// Flag name
        flag_name: String,
        /// Action: on, off, toggle (omit to read)
        action: Option<String>,
        /// Write to a different file
        #[arg(long)]
        output: Option<String>,
        /// Preview changes without writing
        #[arg(long)]
        dry_run: bool,
    },

    /// List enum/flag options for a field
    Options {
        /// Path to a tag file
        file: String,
        /// Field path to an enum or flags field
        path: String,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },

    /// Block element operations
    Block {
        /// Path to a tag file
        file: String,
        /// Field path to a block
        path: String,
        /// Action to perform on the block
        #[arg(value_enum)]
        action: commands::block::BlockAction,
        /// First index argument (insert/duplicate/delete/swap first/move from)
        index: Option<usize>,
        /// Second index argument (swap second / move to)
        index2: Option<usize>,
        /// Write to a different file
        #[arg(long)]
        output: Option<String>,
        /// Preview changes without writing
        #[arg(long)]
        dry_run: bool,
        /// Emit JSON (only meaningful for `count`)
        #[arg(long)]
        json: bool,
    },

    /// List all tag_reference fields in a tag
    Deps {
        /// Path to a tag file
        file: String,
        /// De-duplicate repeated references
        #[arg(long)]
        unique: bool,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },

    /// Walk a directory for tags; filter + list, or summarize by group
    List {
        /// Directory to walk
        dir: String,
        /// Filter by group tag (e.g. "bipd")
        #[arg(long)]
        group: Option<String>,
        /// Only tags whose filename starts with this prefix
        #[arg(long = "starts-with")]
        starts_with: Option<String>,
        /// Only tags whose path contains this substring
        #[arg(long)]
        contains: Option<String>,
        /// Only tags whose filename ends with this suffix (useful for extensions)
        #[arg(long = "ends-with")]
        ends_with: Option<String>,
        /// Only tags whose full path matches this regex
        #[arg(long)]
        regex: Option<String>,
        /// Read candidate tag paths from this file (one per line) instead of walking
        #[arg(long = "from-file")]
        from_file: Option<String>,
        /// Group/extension tally instead of a path list
        #[arg(long)]
        summary: bool,
        /// Sort summary rows by count (desc) instead of name
        #[arg(long = "sort-by-count")]
        sort_by_count: bool,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },

    /// Search across a directory of tags for field values matching a query
    Find {
        /// Directory to walk
        dir: String,
        /// Value substring (or regex, if --regex) to search for
        value: String,
        /// Only search tags of this group
        #[arg(long)]
        group: Option<String>,
        /// Only check fields whose name matches this regex
        #[arg(long = "field-name")]
        field_name: Option<String>,
        /// Interpret `value` as a regex instead of a substring
        #[arg(long)]
        regex: bool,
        /// Emit JSON
        #[arg(long)]
        json: bool,
        /// Fail on any unreadable / malformed tag encountered
        #[arg(long)]
        strict: bool,
    },

    /// Integrity check — flag enum/flag/real/reference anomalies
    Check {
        /// Path to a tag file
        file: String,
        /// Tags root directory; required for tag-reference existence checks
        #[arg(long = "tags-root")]
        tags_root: Option<String>,
        /// Comma-separated subset: enum,flag,real,reference (default: all)
        #[arg(long)]
        only: Option<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
        /// Non-zero exit status on any finding (for CI)
        #[arg(long)]
        strict: bool,
    },

    /// Show tag/cache file header metadata
    Header {
        /// Path to a tag or cache file
        file: String,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },

    /// Diff the layouts of two tag files
    LayoutDiff {
        /// First tag file
        file_a: String,
        /// Second tag file
        file_b: String,
    },

    /// Compare two tags' *values* at every leaf path (distinct from
    /// `layout-diff`, which compares schemas)
    DataDiff {
        /// First tag file
        file_a: String,
        /// Second tag file
        file_b: String,
        /// Optional subtree to restrict both walks to
        #[arg(long)]
        only: Option<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },

    /// Dump a tag's state as `set` commands that reproduce it —
    /// diffable between tags and replayable against another
    Export {
        /// Path to a tag file
        file: String,
        /// Optional field path; only export fields under this subtree
        subtree: Option<String>,
        /// Write to a file instead of stdout
        #[arg(long)]
        output: Option<String>,
    },

    /// Extract a `.bitmap` tag's images as TIFF or DDS (one file per
    /// image; multi-image tags get a directory)
    ExtractBitmap {
        /// Path to a `.bitmap` tag file
        file: String,
        /// Output path. If it ends in `.tif` / `.tiff` / `.dds`,
        /// writes that exact filename (single-image tags only) and
        /// the extension picks the format. Otherwise treated as a
        /// directory: 1-image tags emit `<dir>/<tag_stem>.<ext>`,
        /// N-image tags emit `<dir>/<tag_stem>/<i>.<ext>`.
        /// Default: current directory.
        #[arg(long)]
        output: Option<String>,
        /// Output format. Defaults to `tif` (Tool-importable RGBA8
        /// TIFF). `dds` keeps the original-format DDS dump that's
        /// readable but not Tool-importable.
        #[arg(long, default_value = "tif")]
        format: String,
    },

    /// Extract geometry source files from a tag. Accepts:
    ///
    /// - `.model` (hlmt): render/collision/physics children. Render
    ///   side auto-dispatches JMS or ASS based on
    ///   `instance mesh index >= 0` + non-empty `instance placements[]`
    ///   (the brute, decorators, level objects emit ASS; everything
    ///   else emits JMS). Coll/phys always JMS. `--force jms|ass`
    ///   overrides the render-side decision.
    /// - `.scenario` (scnr): one ASS per `structure_bsps[]` entry,
    ///   with paired `.stli` lighting baked in. Always ASS.
    /// - `.scenario_structure_bsp` (sbsp): a single ASS for that BSP.
    ///   No paired stli (lighting is unreachable without scenario
    ///   context). Always ASS.
    ///
    /// `[KINDS...]` and `--force` are `.model`-only. Passing them with
    /// scenario/sbsp inputs is rejected with an explanatory error.
    ExtractGeometry {
        /// Path to a `.model`, `.scenario`, or `.scenario_structure_bsp` tag.
        file: String,
        /// `.model`-only. Which sub-models to extract: `render`,
        /// `collision`, `physics`, or `all`. Default `all`. Rejected
        /// for scenario / sbsp inputs.
        #[arg(value_parser = ["render", "collision", "physics", "all"])]
        kinds: Vec<String>,
        /// Output directory (default: current directory).
        #[arg(long)]
        output: Option<String>,
        /// Flatten the layout. For `.model`: `<DIR>/<stem>.<kind>.<ext>`.
        /// For `.scenario`: `<DIR>/<scenario>.<bsp>.ass`. No effect on
        /// `.sbsp` (always a single file).
        #[arg(long)]
        flat: bool,
        /// `.model`-only. Force render-side format (`jms` or `ass`),
        /// overriding content-based dispatch. Rejected for scenario /
        /// sbsp inputs (those always emit ASS).
        #[arg(long, value_enum)]
        force: Option<commands::extract_geometry::Force>,
    },

    /// Write the bytes of a single `tag_data` field to a file
    ExtractData {
        /// Path to a tag file
        file: String,
        /// Field path to a `tag_data` field
        path: String,
        /// Output file path. Default: `<tag_stem>.<field_name>.bin`
        /// in the current directory.
        #[arg(long)]
        output: Option<String>,
    },

    /// Decompress and write out the source files baked into a tag's
    /// `info` (import-info) stream. Each `files[i]` carries the
    /// original on-disk path + zlib-compressed bytes of a source asset
    /// the importer consumed (JMS, JMA, TIFF, etc.).
    ExtractImportInfo {
        /// Path to a tag file with an `info` stream
        file: String,
        /// Output directory. Default: `./<tag_stem>/import_info/`.
        #[arg(long)]
        output: Option<String>,
        /// Print the manifest only — don't decompress or write.
        #[arg(long)]
        list: bool,
    },

    /// List the animations in a `model_animation_graph` tag (header
    /// metadata only — no codec decode)
    ListAnimations {
        /// Path to a `.model_animation_graph` tag file
        file: String,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },

    /// Decode animations from a `.model_animation_graph`, or from a
    /// `.model` (hlmt) / object-inheriting tag (.biped, .scenery,
    /// .weapon, .equipment, …) that points at one. Default output is
    /// a JMA-family text file (`.JMM/.JMA/.JMT/...`) re-importable by
    /// Halo content tooling. `--format json` emits the full per-frame
    /// transform table for diagnostics. With no `<anim>` arg, every
    /// animation in the tag is extracted.
    ExtractAnimation {
        /// Path to a `.model_animation_graph`, `.model`, or any
        /// object-inheriting tag (.biped/.weapon/.scenery/…)
        file: String,
        /// Animation index (`definitions/animations[N]`) or name.
        /// Omit to extract every animation in the tag.
        anim: Option<String>,
        /// Output path. Treated as a source-tree root: files land
        /// at `<root>/<tag_stem>/animations/<anim_name>.<EXT>` to
        /// match Tool's `model-animations` layout. Default root is
        /// `.`. A path ending in a JMA-family or `.json` extension
        /// is treated as an exact filename instead (single-anim
        /// only). Single-anim `json` with no `--output` still
        /// prints to stdout for piping.
        #[arg(long)]
        output: Option<String>,
        /// Flatten the layout: emit `<root>/<tag_stem>.<anim_name>.<EXT>`
        /// instead of nested `<root>/<tag_stem>/animations/...` subdirs.
        /// Useful for ad-hoc inspection. Mirrors `extract-geometry --flat`.
        /// Ignored when `--output` is an exact filename.
        #[arg(long)]
        flat: bool,
        /// Output format
        #[arg(long, value_enum, default_value_t = commands::extract_animation::Format::Jma)]
        format: commands::extract_animation::Format,
    },

    /// Attach an empty dependency-list stream to a tag
    AddDependencyList {
        /// Path to a tag file
        file: String,
        /// Write to a different file
        #[arg(long)]
        output: Option<String>,
    },

    /// Drop the dependency-list stream from a tag
    RemoveDependencyList {
        file: String,
        #[arg(long)]
        output: Option<String>,
    },

    /// Rebuild the dependency-list from the tag's own non-`impo`
    /// tag_reference fields. Creates the stream if missing.
    RebuildDependencyList {
        file: String,
        #[arg(long)]
        output: Option<String>,
    },

    /// Attach an empty import-info stream to a tag
    AddImportInfo {
        file: String,
        #[arg(long)]
        output: Option<String>,
    },

    /// Drop the import-info stream from a tag
    RemoveImportInfo {
        file: String,
        #[arg(long)]
        output: Option<String>,
    },

    /// Attach an empty asset-depot-storage stream to a tag
    AddAssetDepotStorage {
        file: String,
        #[arg(long)]
        output: Option<String>,
    },

    /// Drop the asset-depot-storage stream from a tag
    RemoveAssetDepotStorage {
        file: String,
        #[arg(long)]
        output: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let game = cli.game.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "missing required --game/-g flag (e.g. `--game haloreach_mcc`)",
        )
    })?;
    let mut ctx = CliContext::new(game)?;

    match cli.command {
        Commands::Repl { file } => repl::run(&mut ctx, file.as_deref()),
        cmd => dispatch(&mut ctx, cmd, true),
    }
}

/// Execute a parsed command. `reload_tag` controls whether the
/// driver calls [`CliContext::load`] before tag-bound commands:
/// one-shot mode passes `true`, the REPL passes `false` so edits
/// accumulate across commands on a single loaded tag.
pub(crate) fn dispatch(ctx: &mut CliContext, cmd: Commands, reload_tag: bool) -> Result<()> {
    match cmd {
        Commands::Repl { .. } => anyhow::bail!("`repl` is only valid at the top level"),

        Commands::New { group, output } => {
            commands::new::run(ctx, &group, output.as_deref())
        }

        Commands::Inspect { file, path, all, full, json, filter, filter_not, filter_value } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::inspect::run(
                ctx,
                path.as_deref(),
                all,
                full,
                json,
                commands::inspect::InspectFilters {
                    names: filter,
                    excludes: filter_not,
                    value: filter_value,
                },
            )
        }

        Commands::Get { file, path, raw, json, hex } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::get::run(ctx, &path, raw, json, hex)
        }

        Commands::Set { file, path, value, output, dry_run } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::set::run(ctx, &path, &value, output.as_deref(), dry_run)
        }

        Commands::Flag { file, path, flag_name, action, output, dry_run } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::flag::run(ctx, &path, &flag_name, action.as_deref(), output.as_deref(), dry_run)
        }

        Commands::Options { file, path, json } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::options::run(ctx, &path, json)
        }

        Commands::Block { file, path, action, index, index2, output, dry_run, json } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::block::run(ctx, &path, action, index, index2, output.as_deref(), dry_run, json)
        }

        Commands::Deps { file, unique, json } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::deps::run(ctx, unique, json)
        }

        Commands::List {
            dir, group, starts_with, contains, ends_with, regex, from_file, summary, sort_by_count, json,
        } => {
            let filters = commands::list::ListFilters {
                group, starts_with, contains, ends_with, regex, from_file,
            };
            let mode = if json {
                commands::list::OutputMode::Json
            } else if summary {
                commands::list::OutputMode::Summary { sort_by_count }
            } else {
                commands::list::OutputMode::Paths
            };
            commands::list::run(ctx, &dir, filters, mode)
        }

        Commands::Find { dir, value, group, field_name, regex, json, strict } => {
            let filters = commands::find::FindFilters { group, field_name, regex, json, strict };
            commands::find::run(ctx, &dir, &value, filters)
        }

        Commands::Check { file, tags_root, only, json, strict } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::check::run(ctx, tags_root.as_deref(), only.as_deref(), json, strict)
        }

        Commands::Header { file, json } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::header::run(ctx, json)
        }

        Commands::LayoutDiff { file_a, file_b } => commands::layout_diff::run(&file_a, &file_b),

        Commands::DataDiff { file_a, file_b, only, json } => {
            commands::data_diff::run(ctx, &file_a, &file_b, only.as_deref(), json)
        }

        Commands::Export { file, subtree, output } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::export::run(ctx, subtree.as_deref(), output.as_deref())
        }

        Commands::ExtractBitmap { file, output, format } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::extract_bitmap::run(ctx, output.as_deref(), &format)
        }

        Commands::ExtractGeometry { file, kinds, output, flat, force } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::extract_geometry::run(ctx, &kinds, output.as_deref(), flat, force)
        }

        Commands::ExtractData { file, path, output } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::extract_data::run(ctx, &path, output.as_deref())
        }

        Commands::ExtractImportInfo { file, output, list } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::extract_import_info::run(ctx, output.as_deref(), list)
        }

        Commands::ListAnimations { file, json } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::list_animations::run(ctx, json)
        }

        Commands::ExtractAnimation { file, anim, output, flat, format } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::extract_animation::run(ctx, anim.as_deref(), output.as_deref(), flat, format)
        }

        Commands::AddDependencyList { file, output } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::streams::add_dependency_list(ctx, output.as_deref())
        }

        Commands::RemoveDependencyList { file, output } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::streams::remove_dependency_list(ctx, output.as_deref())
        }

        Commands::RebuildDependencyList { file, output } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::streams::rebuild_dependency_list(ctx, output.as_deref())
        }

        Commands::AddImportInfo { file, output } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::streams::add_import_info(ctx, output.as_deref())
        }

        Commands::RemoveImportInfo { file, output } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::streams::remove_import_info(ctx, output.as_deref())
        }

        Commands::AddAssetDepotStorage { file, output } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::streams::add_asset_depot_storage(ctx, output.as_deref())
        }

        Commands::RemoveAssetDepotStorage { file, output } => {
            ensure_loaded(ctx, &file, reload_tag)?;
            commands::streams::remove_asset_depot_storage(ctx, output.as_deref())
        }
    }
}

/// Load `file` into `ctx` (one-shot mode) or verify a tag is already
/// loaded (REPL mode, where `file` is ignored because the REPL's
/// line preprocessor fills it in from [`CliContext::loaded`]).
fn ensure_loaded(ctx: &mut CliContext, file: &str, reload: bool) -> Result<()> {
    if reload {
        ctx.load(file)?;
    } else if ctx.loaded.is_none() {
        anyhow::bail!("no tag loaded (use `open <path>` first)");
    }
    Ok(())
}
