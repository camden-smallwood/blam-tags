//! Halo 5: Guardians extracted `ucsh` tags, XML layouts and fixed-width writes.
//! Produces the ordinary TagFile facade; presentation belongs to the caller.
mod binary;
mod document;
mod model;
mod schema;
mod source;
pub use document::{Document, Field, FieldKind, TagReference};
pub use model::{LoadedTag, TagBacking, read_tag};
pub use schema::{Definitions, Schema, SchemaField, ValueKind};
pub use source::{Entry, Source, is_halo5_file, is_halo5_folder};

#[cfg(test)]
mod tests;
