//! Interactive shell.
//!
//! Wraps the same `Commands` dispatch that `main` uses for one-shot
//! mode, with two differences:
//!
//! 1. A [`CliContext`] persists across commands — one tag load, N
//!    edits, one save. Commands set [`crate::context::LoadedTag::dirty`]
//!    on mutation and the user decides when to `save`.
//! 2. Line preprocessing: tag-bound commands like `get` / `set` /
//!    `flag` don't need a file argument in REPL mode. The loop
//!    prepends [`CliContext::loaded`]'s path automatically before
//!    running the line through clap.
//!
//! REPL-only verbs (`open`, `close`, `save`, `revert`, `exit`, `help`)
//! are handled directly without going through clap.

use std::io::{self, BufRead, Write};

use anyhow::Result;
use clap::Parser;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

use crate::context::CliContext;
use crate::{dispatch, Cli};

const BINARY_NAME: &str = "blam-tag-shell";

pub fn run(ctx: &mut CliContext, initial_tag: Option<&str>) -> Result<()> {
    if let Some(path) = initial_tag {
        ctx.load(path)?;
    }

    let history_path = dirs::home_dir().map(|h| h.join(".blam-tag-shell-history"));

    let mut rl = DefaultEditor::new()?;
    if let Some(p) = &history_path {
        let _ = rl.load_history(p);
    }

    print_banner();

    loop {
        let line = match rl.readline(&prompt_for(ctx)) {
            Ok(line) => line,
            Err(ReadlineError::Eof) => {
                // Ctrl-D / pipe-close. No way to prompt; just warn
                // about unsaved changes and exit cleanly.
                if ctx.loaded.as_ref().is_some_and(|l| l.dirty) {
                    eprintln!("\nwarning: exiting with unsaved changes");
                } else {
                    println!();
                }
                break;
            }
            Err(ReadlineError::Interrupted) => {
                // ^C: clear the current line, keep the REPL running.
                continue;
            }
            Err(e) => return Err(e.into()),
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(trimmed);

        match handle_line(ctx, trimmed) {
            LineOutcome::Continue => {}
            LineOutcome::Exit => break,
            LineOutcome::Error(e) => {
                // clap errors carry their own `error:` prefix; anyhow
                // errors from our code don't. Print both uniformly by
                // letting each error render itself.
                eprintln!("{}", e);
            }
        }
    }

    if let Some(p) = &history_path {
        let _ = rl.save_history(p);
    }

    Ok(())
}

enum LineOutcome {
    Continue,
    Exit,
    Error(anyhow::Error),
}

/// A verb the REPL handles itself instead of passing to clap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReplVerb {
    Exit,
    Help,
    Open,
    Close,
    Save,
    Revert,
    EditBlock,
    Back,
    ExitTo,
    Pwd,
    Repl,
}

/// Every word the REPL intercepts before clap sees the line, aliases
/// included. `parity.toml` lists each one; a test checks the two agree.
pub(crate) const REPL_VERBS: &[(&str, ReplVerb)] = &[
    ("exit", ReplVerb::Exit),
    ("quit", ReplVerb::Exit),
    ("help", ReplVerb::Help),
    ("?", ReplVerb::Help),
    ("open", ReplVerb::Open),
    ("close", ReplVerb::Close),
    ("save", ReplVerb::Save),
    ("revert", ReplVerb::Revert),
    ("edit-block", ReplVerb::EditBlock),
    ("cd", ReplVerb::EditBlock),
    ("back", ReplVerb::Back),
    ("exit-to", ReplVerb::ExitTo),
    ("pwd", ReplVerb::Pwd),
    ("repl", ReplVerb::Repl),
];

fn repl_verb(word: &str) -> Option<ReplVerb> {
    REPL_VERBS.iter().find(|(name, _)| *name == word).map(|(_, verb)| *verb)
}

fn handle_line(ctx: &mut CliContext, line: &str) -> LineOutcome {
    let Some(words) = shlex::split(line) else {
        return LineOutcome::Error(anyhow::anyhow!("unbalanced quotes"));
    };
    if words.is_empty() {
        return LineOutcome::Continue;
    }

    let verb = words[0].as_str();
    let rest = &words[1..];

    let Some(verb) = repl_verb(verb) else {
        return match dispatch_via_clap(ctx, &words) {
            Ok(()) => LineOutcome::Continue,
            Err(e) => LineOutcome::Error(e),
        };
    };

    let result: Result<_, anyhow::Error> = match verb {
        ReplVerb::Exit => {
            return if rest.is_empty() || rest[0] != "--force" {
                if confirm_discard_dirty(ctx, "exit") { LineOutcome::Exit } else { LineOutcome::Continue }
            } else {
                LineOutcome::Exit
            };
        }
        ReplVerb::Help => { print_help(ctx); return LineOutcome::Continue; }
        ReplVerb::Open => repl_open(ctx, rest),
        ReplVerb::Close => repl_close(ctx),
        ReplVerb::Save => repl_save(ctx, rest),
        ReplVerb::Revert => repl_revert(ctx),
        ReplVerb::EditBlock => repl_edit_block(ctx, rest),
        ReplVerb::Back => repl_back(ctx),
        ReplVerb::ExitTo => repl_exit_to(ctx, rest),
        ReplVerb::Pwd => { print_pwd(ctx); Ok(()) }
        ReplVerb::Repl => Err(anyhow::anyhow!("already in a REPL")),
    };

    match result {
        Ok(()) => LineOutcome::Continue,
        Err(e) => LineOutcome::Error(e),
    }
}

fn dispatch_via_clap(ctx: &mut CliContext, words: &[String]) -> Result<()> {
    // REPL parsing has to disambiguate two flavors of verb without
    // hardcoding a list:
    //
    //  - Tag-bound verbs (`inspect`, `get`, `set`, `flag`, …) require
    //    a `<file>` positional. The REPL fills it in from the loaded
    //    tag so users don't retype the path on every command.
    //  - Corpus verbs (`list`, `find`, `layout-diff`, …) take a
    //    different first positional (`<dir>`/`<file_a>`/etc.) and
    //    don't accept the loaded path.
    //
    // When the user types `inspect materials[0]`, clap will *happily*
    // parse that as `file="materials[0]", path=None` — which silently
    // dumps the root instead of drilling into the element. To avoid
    // that trap we prefer the injected form when a tag is loaded:
    // try `<verb> <loaded-path> <user-args...>` first, and fall back
    // to the as-typed parse only if injection fails (which is what
    // happens for corpus verbs whose signature can't accept the
    // extra positional).
    let raw: Vec<String> = std::iter::once(BINARY_NAME.to_string())
        .chain(words.iter().cloned())
        .collect();

    if let Some(loaded) = ctx.loaded.as_ref() {
        let mut injected = raw.clone();
        injected.insert(2, loaded.path.to_string_lossy().into_owned());
        if let Ok(cli) = Cli::try_parse_from(&injected) {
            return dispatch(ctx, cli.command, false);
        }
    }

    match Cli::try_parse_from(&raw) {
        Ok(cli) => dispatch(ctx, cli.command, false),
        Err(e) => Err(e.into()),
    }
}

fn repl_open(ctx: &mut CliContext, rest: &[String]) -> Result<()> {
    let path = rest.first().ok_or_else(|| anyhow::anyhow!("usage: open <tag-path>"))?;
    if !confirm_discard_dirty(ctx, "open") {
        return Ok(());
    }
    ctx.load(path)?;
    Ok(())
}

fn repl_close(ctx: &mut CliContext) -> Result<()> {
    if ctx.loaded.is_none() {
        anyhow::bail!("no tag loaded");
    }
    if !confirm_discard_dirty(ctx, "close") {
        return Ok(());
    }
    ctx.loaded = None;
    Ok(())
}

fn repl_save(ctx: &mut CliContext, rest: &[String]) -> Result<()> {
    let loaded = ctx.loaded_mut("save")?;
    let dest = rest.first().map(std::path::Path::new);
    let target = loaded.save(dest)?;
    println!("saved to {}", target.display());
    Ok(())
}

fn repl_revert(ctx: &mut CliContext) -> Result<()> {
    let path = {
        let loaded = ctx.loaded("revert")?;
        loaded.path.clone()
    };
    ctx.load(path)?;
    println!("reverted to on-disk contents");
    Ok(())
}

fn repl_edit_block(ctx: &mut CliContext, rest: &[String]) -> Result<()> {
    let target = rest.first().ok_or_else(|| anyhow::anyhow!("usage: edit-block <path>"))?;

    // Unix-cd semantics: leading `/` resets to absolute. The nav
    // stores one segment per `/`-separated component so `back`
    // always undoes exactly one step.
    let (absolute, body) = match target.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, target.as_str()),
    };
    let new_segments: Vec<String> = body.split('/').filter(|s| !s.is_empty()).map(String::from).collect();
    if new_segments.is_empty() {
        anyhow::bail!("empty edit-block target");
    }

    let prospective: Vec<String> = if absolute {
        new_segments.clone()
    } else {
        ctx.nav.iter().cloned().chain(new_segments.iter().cloned()).collect()
    };
    let prospective_path = prospective.join("/");

    let loaded = ctx.loaded("edit-block")?;
    let root = loaded.tag.root();
    let field = root
        .field_path(&prospective_path)
        .ok_or_else(|| anyhow::anyhow!("field '{}' not found", prospective_path))?;
    let navigable = field.as_struct().is_some()
        || field.as_block().is_some()
        || field.as_array().is_some()
        || field.as_resource().and_then(|r| r.as_struct()).is_some();
    if !navigable {
        if let Some(resource) = field.as_resource() {
            anyhow::bail!(
                "field '{}' is a {:?} pageable_resource — only Exploded resources can be navigated",
                prospective_path,
                resource.kind(),
            );
        }
        anyhow::bail!("field '{}' is not a navigable struct / block / array / pageable_resource", prospective_path);
    }

    ctx.nav = prospective;
    Ok(())
}

fn repl_back(ctx: &mut CliContext) -> Result<()> {
    if ctx.nav.is_empty() {
        anyhow::bail!("already at the tag root");
    }
    ctx.nav.pop();
    Ok(())
}

fn repl_exit_to(ctx: &mut CliContext, rest: &[String]) -> Result<()> {
    let target = rest.first().ok_or_else(|| anyhow::anyhow!("usage: exit-to <segment|root|tag>"))?;

    if matches!(target.as_str(), "root" | "tag" | "/") {
        ctx.nav.clear();
        return Ok(());
    }

    // Pop entries off the tail until one matches `target` (exact or
    // substring), leaving that entry on the stack. Fails cleanly if
    // nothing matches.
    let mut saved = ctx.nav.clone();
    while let Some(last) = saved.last() {
        if last == target || last.contains(target.as_str()) {
            ctx.nav = saved;
            return Ok(());
        }
        saved.pop();
    }
    anyhow::bail!("no nav segment matching '{target}'");
}

fn print_pwd(ctx: &CliContext) {
    if ctx.nav.is_empty() {
        println!("/");
    } else {
        println!("/{}", ctx.nav.join("/"));
    }
}

fn prompt_for(ctx: &CliContext) -> String {
    let game = ctx.game.as_deref().unwrap_or("blam");
    match &ctx.loaded {
        None => format!("{game}> "),
        Some(loaded) => {
            let name = loaded
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?");
            let dirty = if loaded.dirty { "*" } else { "" };
            if ctx.nav.is_empty() {
                format!("{game} :: {name}{dirty}> ")
            } else {
                format!("{game} :: {name}{dirty}/{nav}> ", nav = ctx.nav.join("/"))
            }
        }
    }
}

fn print_banner() {
    println!("blam-tag-shell REPL — type `help` for commands, Ctrl-D to exit");
}

fn print_help(ctx: &CliContext) {
    println!("Session verbs:");
    println!("  open <path>         load a tag");
    println!("  close               close the current tag");
    println!("  save [path]         write the current tag (to `path` or back to the source)");
    println!("  revert              reload the current tag from disk, discarding edits");
    println!("  help                show this message");
    println!("  exit / quit         leave the REPL");
    println!();
    println!("Navigation:");
    println!("  edit-block <path>   push a sub-struct / block-element / array-element onto nav");
    println!("                      (aliased `cd`; leading `/` resets to absolute)");
    println!("  back                pop one level");
    println!("  exit-to <name>      pop until the named segment is the tail; `exit-to root` clears");
    println!("  pwd                 show the current nav path");
    println!();
    println!("Tag commands (omit the file arg, interpreted relative to nav):");
    println!("  header, inspect, get, set, flag, options, block, deps, export, check");
    println!();
    println!("Directory / corpus commands:");
    println!("  list <dir> [...]        walk a directory for tags");
    println!("  find <dir> <value>      deep value search");
    println!("  layout-diff <a> <b>     compare two tags' schemas");
    println!("  data-diff <a> <b>       compare two tags' values");
    println!();
    if let Some(loaded) = &ctx.loaded {
        println!(
            "Loaded: {} ({})",
            loaded.path.display(),
            if loaded.dirty { "unsaved changes" } else { "clean" },
        );
        if !ctx.nav.is_empty() {
            println!("Nav:    /{}", ctx.nav.join("/"));
        }
    } else {
        println!("No tag loaded.");
    }
}

/// Return `true` if the caller should proceed with the discarding
/// operation (exit, open-new, close). Prompts the user if the loaded
/// tag is dirty; treats non-interactive stdin (piped) as "proceed".
fn confirm_discard_dirty(ctx: &CliContext, action: &str) -> bool {
    let Some(loaded) = &ctx.loaded else { return true };
    if !loaded.dirty {
        return true;
    }
    print!(
        "`{}` has unsaved changes. {} anyway? [y/N] ",
        loaded.path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
        action,
    );
    let _ = io::stdout().flush();

    let stdin = io::stdin();
    let mut line = String::new();
    if stdin.lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}
