//! `set` — write a field's value from a string. The central mutation
//! point for scalar edits; `flag` and `block` are specialized forms
//! of the same pattern. Two-pass design: read the previous value and
//! validate the parse on the immutable handle first, then only enter
//! the mutable path for real runs. Emits `(was X)` so diffs are
//! self-describing.

use std::path::Path;

use anyhow::{Context, Result};
use blam_tags::TagSetError;

use crate::context::CliContext;
use crate::format::format_value;
use crate::parse::{parse_field_value, ParseError};

pub fn run(
    ctx: &mut CliContext,
    path: &str,
    value: &str,
    output: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    let resolved = ctx.resolve_path(path);

    // First, capture the previous value (for the "(was …)" suffix)
    // and parse the new value. Both go through the immutable handle
    // so dry-run is touch-free and the borrow checker is happy when
    // we re-borrow `ctx` mutably below.
    let (previous, parsed) = {
        let loaded = ctx.loaded("set")?;
        let field = loaded
            .tag
            .root()
            .field_path(&resolved)
            .with_context(|| format!("field '{}' not found", resolved))?;
        let previous = field.value().as_ref().map(|v| format_value(ctx, v, false));
        let parsed = parse_field_value(ctx, &field, value).map_err(parse_error_to_anyhow)?;
        (previous, parsed)
    };

    let was_phrase = previous
        .map(|p| format!(" (was {p})"))
        .unwrap_or_default();
    let core = format!("set {resolved} = {value}{was_phrase}");

    if dry_run {
        println!("(dry run) would {core}");
        return Ok(());
    }

    // Then apply via mutable borrow. No `ctx` access here — the
    // typed `field.set(value)` only needs the layout the field
    // already holds.
    let loaded = ctx.loaded_mut("set")?;
    {
        let mut root = loaded.tag.root_mut();
        let mut field = root
            .field_path_mut(&resolved)
            .with_context(|| format!("field '{}' not found", resolved))?;
        field.set(parsed).map_err(set_error_to_anyhow)?;
    }
    loaded.dirty = true;

    let commit = loaded.commit(output.map(Path::new))?;
    println!("{core}");
    if commit.redirected {
        println!("saved to {}", commit.target.display());
    }

    Ok(())
}

fn parse_error_to_anyhow(e: ParseError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

fn set_error_to_anyhow(e: TagSetError) -> anyhow::Error {
    match e {
        TagSetError::NotAssignable => anyhow::anyhow!("cannot set container field types directly"),
        TagSetError::TypeMismatch { expected, got } => {
            anyhow::anyhow!("type mismatch: expected {expected}, got {got}")
        }
        other => anyhow::anyhow!("{other}"),
    }
}
