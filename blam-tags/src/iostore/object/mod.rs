//! Layer 3 — one export's payload: the unversioned property block laid out by
//! the `.usmap` schema, and the natively serialized tails each class in the
//! inheritance chain appends after it.
//!
//! This is the layer that understands UE's serialization rules. It takes a byte
//! range from [`super::package`] and turns it into values. See
//! [`unversioned`] for the map of which module holds what.

pub mod archive;
pub mod block;
pub mod common;
pub mod edit;
pub mod export;
mod limits;
pub mod property;
pub mod reflect;
pub mod structs;
pub mod tails;
pub mod text;
pub mod unversioned;
pub mod usmap;
pub mod value;
