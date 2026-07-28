//! Layer 1 — the bytes of an IoStore container: the `.utoc` index, the `.ucas`
//! data blocks, the legacy `.pak` sibling, and the writer that emits an
//! override triplet.
//!
//! Everything here is about locating and (de)compressing *chunks*. It knows
//! nothing about what a chunk contains; that is [`super::package`]'s business.

pub mod header;
pub mod oodle;
pub mod pak;
pub mod writer;
