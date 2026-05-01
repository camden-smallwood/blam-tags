//! Subcommand implementations.
//!
//! One module per CLI verb. Each exposes a `pub fn run(...)` invoked
//! from `main::dispatch`. Tag-bound commands take `&mut CliContext`
//! (the tag is loaded into `ctx.loaded` before dispatch); read-only
//! commands that work on raw paths (`list`, `layout_diff`) take their
//! arguments directly.

pub mod block;
pub mod check;
pub mod data_diff;
pub mod deps;
pub mod export;
pub mod extract_animation;
pub mod extract_bitmap;
pub mod extract_data;
pub mod extract_geometry;
pub mod extract_import_info;
pub mod find;
pub mod flag;
pub mod get;
pub mod header;
pub mod inspect;
pub mod layout_diff;
pub mod list;
pub mod list_animations;
pub mod new;
pub mod options;
pub mod set;
pub mod streams;
