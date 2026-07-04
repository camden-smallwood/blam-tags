//! Error type for the [`extract`](super) orchestration.
//!
//! The extraction glue calls into several subsystems (animation decode,
//! ASS/JMS builders, tag resolution, filesystem I/O) that each surface
//! their own error type. `ExtractError` unifies them so the directory
//! walkers can use `?` freely, and implements [`std::error::Error`] so
//! `anyhow`-based callers (blam-tag-shell) can convert it transparently.

use std::fmt;

use crate::animation::AnimationError;
use crate::ass::AssError;
use crate::jms::JmsError;

/// A failure while extracting animations or geometry to a directory.
#[derive(Debug)]
pub enum ExtractError {
    /// Filesystem I/O failure (create dir/file, write).
    Io(std::io::Error),
    /// Animation decode / graph-walk failure.
    Animation(AnimationError),
    /// ASS build/write failure.
    Ass(AssError),
    /// JMS build/write failure.
    Jms(JmsError),
    /// A [`TagResolver`](super::TagResolver) could not produce a
    /// referenced tag (path missing, read failed, cache miss, …).
    Resolve(String),
    /// Orchestration-level failure carrying a human-readable message
    /// (e.g. "scenario has zero structure_bsps entries").
    Message(String),
}

impl ExtractError {
    /// Construct a [`ExtractError::Message`].
    pub fn msg(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }

    /// Construct a [`ExtractError::Resolve`].
    pub fn resolve(message: impl Into<String>) -> Self {
        Self::Resolve(message.into())
    }
}

impl fmt::Display for ExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Animation(e) => write!(f, "{e}"),
            Self::Ass(e) => write!(f, "{e}"),
            Self::Jms(e) => write!(f, "{e}"),
            Self::Resolve(m) => write!(f, "{m}"),
            Self::Message(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ExtractError {}

impl From<std::io::Error> for ExtractError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<AnimationError> for ExtractError {
    fn from(e: AnimationError) -> Self {
        Self::Animation(e)
    }
}

impl From<AssError> for ExtractError {
    fn from(e: AssError) -> Self {
        Self::Ass(e)
    }
}

impl From<JmsError> for ExtractError {
    fn from(e: JmsError) -> Self {
        Self::Jms(e)
    }
}
