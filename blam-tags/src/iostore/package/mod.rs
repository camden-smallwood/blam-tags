//! Layer 2 — one cooked Zen package: its summary, name map, import/export maps
//! and dependency bundles.
//!
//! A package is the unit a container stores and the unit an export lives in. It
//! knows the *extent* of each export's serial data but nothing about the bytes
//! inside it; that is [`super::object`]'s business.

pub mod builder;
pub mod name_map;
pub mod script_objects;
pub mod ser;
pub mod ue_types;
pub mod zen;
