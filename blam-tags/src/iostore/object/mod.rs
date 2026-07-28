//! Layer 3 — one export's payload: the unversioned property block laid out by
//! the `.usmap` schema, and the natively serialized tails each class in the
//! inheritance chain appends after it.
//!
//! This is the layer that understands UE's serialization rules. It takes a byte
//! range from [`super::package`] and turns it into values.

pub mod unversioned;
pub mod usmap;
