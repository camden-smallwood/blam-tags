//! Directory-oriented extraction orchestration for tag → source-file
//! export, shared by `blam-tag-shell` and downstream tools (e.g. Baboon).
//!
//! The underlying primitives — [`Pose::write_jma`](crate::Pose::write_jma),
//! [`AnimationClip::decode`](crate::AnimationClip),
//! [`AssFile::from_scenario_structure_bsp`](crate::AssFile),
//! [`JmsFile`](crate::JmsFile) — have always lived in the library. What
//! lived only in the CLI was the *glue*: resolving a jmad's referenced
//! render_model to build the rest pose, picking the JMA-family kind per
//! animation, composing overlays/replacements against the right base,
//! walking a scenario's `structure_bsps[]`, layering `.stli` lights, and
//! writing everything into Tool's source-tree folder layout.
//!
//! This module lifts that glue behind a small [`TagResolver`] abstraction
//! so any caller — filesystem, monolithic cache, classic-format-aware —
//! can drive the same export without shelling out to the CLI.
//!
//! - [`animation::animations_to_dir`] — every animation in a jmad / .model
//!   / object-inheriting tag / Halo CE `model_animations`, as JMA-family
//!   files under `<out>/<stem>/animations/`.
//! - [`geometry::scenario_geometry_to_dir`] — one ASS (H2/H3) or render +
//!   collision JMS (Halo CE) per structure BSP, under `<out>/<stem>/structure/`.

mod error;

pub mod animation;
pub mod geometry;

pub use error::ExtractError;

use crate::TagFile;

/// Resolves a tag_reference (relative path + group) to a fully-read
/// [`TagFile`]. Callers own the resolution strategy: filesystem under a
/// `tags/` root, a monolithic cache, classic (Halo CE / Halo 2) layout
/// handling, etc. Both the group's friendly extension (for filesystem
/// path building) and its 4CC group tag (for cache lookups) are supplied
/// so the implementation can use whichever it needs.
pub trait TagResolver {
    /// Read the tag at `reference` (a backslash-delimited tag path with no
    /// extension, as stored in a tag_reference field) whose group is
    /// `group_ext` / `group_tag`.
    fn resolve(
        &self,
        reference: &str,
        group_ext: &str,
        group_tag: u32,
    ) -> Result<TagFile, ExtractError>;
}
