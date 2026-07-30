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
pub mod hand_written;
mod limits;
pub mod native;
pub mod native_bool;
pub mod ue_struct;
pub mod property;
pub mod reflect;
pub mod structs;
pub mod tail_models;
pub mod tails;
pub mod text;
pub mod unversioned;
pub mod usmap;
pub mod value;

/// A reader over a standalone span, for tools that already hold a tail.
pub fn archive_reader<'a>(bytes: &'a [u8], names: &'a [String]) -> archive::Reader<'a> {
    archive::Reader::new(bytes, names)
}

/// The `EManagedArrayType` name for a type id, or `"?"`.
pub fn managed_array_type_name(id: i32) -> &'static str {
    tails::MANAGED_ARRAY_TYPES.get(id.max(0) as usize).copied().unwrap_or("?")
}
